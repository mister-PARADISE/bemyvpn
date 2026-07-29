import SwiftUI
import UIKit
import NetworkExtension
import BmvFFI

@main
struct BeMyVPNApp: App {
    @StateObject private var app = AppState()
    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(app)
                .preferredColorScheme(.dark)
                .onAppear { app.start(); fixCatalystWindow() }
                .onOpenURL { app.openDeepLink($0) }
        }
    }
}

/// Mac Catalyst: жёстко фиксируем окно под форму ТЕЛЕФОНА (как на iPhone) —
/// неизменяемый размер + спрятанная тулбар-полоска заголовка. На iOS — no-op.
func fixCatalystWindow() {
    #if targetEnvironment(macCatalyst)
    // Крупное окно 1280×(под экран). Клампим к экрану, чтобы всегда влезало.
    let apply = {
        for scene in UIApplication.shared.connectedScenes {
            guard let ws = scene as? UIWindowScene else { continue }
            let bounds = ws.screen.bounds
            // ВЕРТИКАЛЬНО: тянем в ВЫСОТУ (до 1280, но не выше экрана), ширину
            // держим узкой — портрет как телефон, просто высокий.
            let h = min(1280, bounds.height - 40)
            let w = min(480, bounds.width - 40)
            let size = CGSize(width: w, height: h)
            ws.sizeRestrictions?.minimumSize = size
            ws.sizeRestrictions?.maximumSize = size     // min == max → официальное ограничение
            ws.titlebar?.titleVisibility = .hidden
            ws.titlebar?.toolbar = nil
        }
        lockCatalystResize()                            // добиваем те самые «пара пикселей»
    }
    apply()
    // Повторяем на следующем витке runloop: на первом onAppear окно/сцена иногда
    // ещё не финализированы, и ограничение «съезжало».
    DispatchQueue.main.async(execute: apply)
    #endif
}

/// Mac Catalyst: ПОЛНОСТЬЮ убираем возможность тянуть окно за края. sizeRestrictions
/// (официальный API) оставляет люфт в 1–2px и живой resize-курсор у краёв; убрать
/// это можно только сняв флаг `resizable` у нижележащего NSWindow. AppKit в Catalyst
/// напрямую недоступен, поэтому дотягиваемся через NSApplication по KVC (штатный
/// приём Catalyst-приложений). Всё в guard/optional — если Apple что-то поменяет,
/// просто ничего не произойдёт (останется официальное min==max).
func lockCatalystResize() {
    #if targetEnvironment(macCatalyst)
    guard let appClass = NSClassFromString("NSApplication") as? NSObject.Type,
          let shared = appClass.value(forKey: "sharedApplication") as? NSObject,
          let windows = shared.value(forKey: "windows") as? [NSObject] else { return }
    let resizableBit: UInt = 1 << 3   // NSWindowStyleMask.resizable
    for w in windows {
        if let mask = w.value(forKey: "styleMask") as? UInt, mask & resizableBit != 0 {
            w.setValue(mask & ~resizableBit, forKey: "styleMask")
        }
    }
    #endif
}

enum Tab { case server, vpn, host }

/// Тактильная отдача на ключевые моменты — маленький штрих качества.
enum Haptics {
    static func success() { UINotificationFeedbackGenerator().notificationOccurred(.success) }
    static func tap() { UIImpactFeedbackGenerator(style: .medium).impactOccurred() }
}

/// Слова под текущую платформу — один код iOS+macOS (Catalyst), но тексты,
/// где важна платформа, подставляются свои. На iOS — «телефон/iPhone», на
/// macOS-сборке (Catalyst) — «компьютер/этот Mac».
enum Platform {
    static var device: String {
        #if targetEnvironment(macCatalyst)
        return "компьютер"
        #else
        return "телефон"
        #endif
    }
    static var deviceName: String {
        #if targetEnvironment(macCatalyst)
        return "этом Mac"
        #else
        return "iPhone"
        #endif
    }
}

@MainActor
final class AppState: ObservableObject {
    // навигация
    @Published var tab: Tab = .vpn

    // сервер / каталог
    @Published var coordinator = UserDefaults.standard.string(forKey: "coordinator") ?? Core.defaultCoordinator
    @Published var hosts: [Host] = []
    @Published var serverOnline: Bool? = nil
    @Published var myIp = ""
    @Published var ping = 0
    @Published var checking = false

    // гость / VPN
    @Published var vpnState: Int32 = 0            // 0 выкл · 1 подключаюсь · 2 канал поднят · 3 ошибка
    @Published var connectedTo: String? = nil
    @Published var connectedSince: Date? = nil
    @Published var resolvedHost: Host? = nil
    @Published var expandedId: String? = nil

    // хост
    @Published var hosting = false
    @Published var starting = false
    @Published var hostCode = UserDefaults.standard.string(forKey: "host_code") ?? ""
    @Published var hostSig = UserDefaults.standard.string(forKey: "host_sig") ?? ""
    @Published var hostName = UserDefaults.standard.string(forKey: "host_name") ?? UIDevice.current.name
    @Published var hostMax = UserDefaults.standard.object(forKey: "host_max") as? Int ?? 8
    @Published var hostPassword = UserDefaults.standard.string(forKey: "host_pw") ?? ""
    @Published var hostProtocol = UserDefaults.standard.string(forKey: "host_proto") ?? "noise-obfs"
    @Published var hostPublic = UserDefaults.standard.object(forKey: "host_public") as? Bool ?? true
    @Published var hostError: String? = nil
    @Published var myHostInfo: Host? = nil        // своя запись в каталоге (гости/IP/…)
    @Published var hostStartedAt: Date? = nil

    // недавние (для текущего координатора)
    @Published var recent: [String] = []

    // Реальный VPN-туннель (Network Extension). На симуляторе не используется.
    let tunnel = TunnelManager()

    private var watchTask: Task<Void, Never>?
    private var checkTask: Task<Void, Never>?
    private var statusTask: Task<Void, Never>?
    private var hostInfoTask: Task<Void, Never>?
    private var applyTask: Task<Void, Never>?

    private var hostToken: String {
        if let t = UserDefaults.standard.string(forKey: "host_token") { return t }
        let t = UUID().uuidString
        UserDefaults.standard.set(t, forKey: "host_token")
        return t
    }

    // недавние серверы (история координаторов)
    @Published var serverHistory: [String] = UserDefaults.standard.stringArray(forKey: "server_history") ?? []

    func start() {
        Task.detached { GeoFlags.load() }   // база IP→страна в фоне
        loadRecent(); startWatch(); startStatus(); checkServer()
        if TunnelManager.available {
            tunnel.onStatus = { [weak self] in self?.applyTunnelStatus($0) }
            Task { await tunnel.prime() }
        }
    }

    /// Статус системного VPN → состояние UI (устройство, реальный туннель).
    private func applyTunnelStatus(_ status: NEVPNStatus) {
        withAnimation {
            switch status {
            case .connecting, .reasserting:
                vpnState = 1
            case .connected:
                vpnState = 2
                cancelQuick()   // подключились — прекращаем перебор кандидатов
                // Успех-хаптик ТОЛЬКО на первое подключение (не на каждый
                // авто-реконнект — иначе в машине телефон бы дребезжал).
                if connectedSince == nil { connectedSince = Date(); Haptics.success() }
                // В «недавние» — ТОЛЬКО по факту реального подключения.
                if let id = connectedTo { addRecent(id) }
                // Туннель забрал маршрут — прежний WS к координатору повис.
                // Переустанавливаем каталог-фид через туннель, иначе счётчик
                // гостей (и весь список) обновляется с задержкой на reconnect.
                startWatch()
            case .disconnected, .invalid:
                vpnState = 0; connectedTo = nil; connectedSince = nil
                startWatch()   // и обратно на прямой маршрут
            case .disconnecting:
                break
            @unknown default:
                break
            }
        }
    }

    func addServerHistory(_ url: String) {
        var h = serverHistory.filter { $0 != url }; h.insert(url, at: 0); h = Array(h.prefix(6))
        serverHistory = h; UserDefaults.standard.set(h, forKey: "server_history")
    }
    func removeServerHistory(_ url: String) {
        serverHistory = serverHistory.filter { $0 != url }; UserDefaults.standard.set(serverHistory, forKey: "server_history")
    }

    /// Открыть по deep-link (bemyvpn://CODE или bemyvpn://connect?code=CODE).
    func openDeepLink(_ url: URL) {
        var code = url.host ?? ""
        if code == "connect", let c = URLComponents(url: url, resolvingAgainstBaseURL: false)?
            .queryItems?.first(where: { $0.name == "code" })?.value { code = c }
        if !code.isEmpty { tab = .vpn; connectByCode(code) }
    }

    // ── каталог ──
    func startWatch() {
        watchTask?.cancel()
        let coord = coordinator
        watchTask = Task {
            var version: UInt64 = 0
            while !Task.isCancelled {
                let (v, hs) = await bg { Core.listWatch(coord, since: version) }
                if Task.isCancelled { break }
                if v > 0 { version = v; hosts = hs; serverOnline = true  }
                else { serverOnline = false; try? await Task.sleep(nanoseconds: 3_000_000_000) }
            }
        }
    }

    // Опрос реального статуса ядра — только терминальные состояния туннеля (платный
    // билд). На free-сборке «канал поднят» ведёт connect() сам; сюда не лезем.
    func startStatus() {
        statusTask?.cancel()
        statusTask = Task {
            while !Task.isCancelled {
                let s = await bg { Core.vpnStatus() }
                if s == 3 { vpnState = 0; connectedTo = nil; connectedSince = nil }
                try? await Task.sleep(nanoseconds: 900_000_000)
            }
        }
    }

    func checkServer() {
        checkTask?.cancel()
        let coord = coordinator
        checking = true
        checkTask = Task {
            let t0 = Date()
            let ok = await bg { Core.health(coord) }
            let ms = Int(Date().timeIntervalSince(t0) * 1000)
            let ip = ok ? await bg { Core.myIp(coord) } : ""
            // Ответ ПРО ПРОШЛЫЙ сервер не должен затирать свежий результат.
            // health() к недоступному адресу висит десятки секунд на таймауте
            // TCP и возвращается уже после того, как пользователь переключился
            // обратно на живой сервер — без этой проверки поздний `false`
            // ставил «нет связи» и прочерки поверх успешного ответа.
            // Именно guard, а не только cancel: bg-вызов блокирующий и отмену
            // не слышит, отменяется лишь ожидание.
            guard !Task.isCancelled, coord == coordinator else { return }
            serverOnline = ok; ping = ms; myIp = ip; checking = false
            if ok { addServerHistory(coord) }
        }
    }

    func saveCoordinator(_ url: String) {
        var u = url.trimmingCharacters(in: .whitespaces)
        if u.hasSuffix("/") { u.removeLast() }
        guard u.hasPrefix("http") else { return }
        coordinator = u
        UserDefaults.standard.set(u, forKey: "coordinator")
        hosts = []; resolvedHost = nil
        loadRecent(); startWatch(); checkServer()
    }

    // ── гость ──
    func hostById(_ id: String) -> Host? {
        hosts.first { $0.id == id } ?? (resolvedHost?.id == id ? resolvedHost : nil)
    }

    // ── Умный «Старт»: очередь кандидатов с фолбэком по таймауту ──
    /// Поводок для кандидата, ПОСЛЕ которого есть кого попробовать ещё. Внутри
    /// ядра на пробитие NAT отведено 12с — щедро, потому что двум мобильным NAT
    /// столько и нужно. Но пока в очереди ждут живые хосты, сидеть эти 12с у
    /// молчащего незачем: дешевле взять следующего.
    private static let quickAttempt: TimeInterval = 5
    /// Поводок для ПОСЛЕДНЕЙ попытки: запасных больше нет, поэтому даём пробитию
    /// доработать полностью. Прежние общие 8с были МЕНЬШЕ окна пробития, и у
    /// человека за строгим NAT «Старт» не срабатывал в принципе, сколько ни жми.
    private static let quickLast: TimeInterval = 15
    /// Сколько лучших кандидатов вообще пробуем. Без потолка перебор шёл бы по
    /// всему каталогу — при сотне свободных хостов это минуты ожидания. Не подошёл
    /// никто из первой пятёрки — проблема не в хостах.
    private static let quickMax = 5
    private var qcQueue: [Host] = []   // оставшиеся кандидаты текущего «Старта»
    private var qcGen = 0              // поколение попытки (инвалидирует старые таймеры)

    /// Код страны хоста (как в карточке: по IP, иначе по полю country).
    private func hostCountryCode(_ h: Host) -> String? {
        GeoFlags.countryOf(h.ip) ?? (h.country.isEmpty ? nil : h.country.uppercased())
    }

    /// Кандидаты для авто-подключения, УПОРЯДОЧЕННЫЕ: сначала хосты в ЧУЖОЙ
    /// стране (VPN обычно нужен ради другой страны), затем — с бОльшим числом
    /// свободных слотов. Страна клиента — регион устройства (оффлайн-эвристика).
    private func quickConnectCandidates() -> [Host] {
        let mine = Locale.current.region?.identifier.uppercased()
        return hosts.filter { $0.usable && !$0.hasPassword && $0.id != hostCode }
            .sorted { a, b in
                if let mine {
                    let af = (hostCountryCode(a) ?? "") != mine
                    let bf = (hostCountryCode(b) ?? "") != mine
                    if af != bf { return af }          // чужая страна — раньше
                }
                return (a.max - a.guests) > (b.max - b.guests)  // больше свободных слотов
            }
            .prefix(Self.quickMax)
            .map { $0 }
    }

    /// «Старт»: пробуем кандидатов по очереди, пока один не подключится.
    func quickConnect() {
        guard vpnState == 0 else { return }
        Haptics.tap()
        qcQueue = quickConnectCandidates()
        tryNextQuick()
    }

    /// Взять следующего кандидата и подключаться; на таймаут — рекурсивно дальше.
    private func tryNextQuick() {
        guard !qcQueue.isEmpty else {
            if vpnState != 2 { withAnimation { vpnState = 0; connectedTo = nil } }
            return
        }
        let host = qcQueue.removeFirst()
        let isLast = qcQueue.isEmpty
        qcGen += 1
        let gen = qcGen
        connect(host)
        // Не подключились за отведённое → гасим и берём следующего. Последнему
        // даём полный бюджет: запасных за ним нет, обрывать пробитие на полпути
        // уже незачем. Проверка поколения: успех/стоп/ручное подключение
        // увеличивают qcGen и глушат этот таймер, чтобы он не оборвал живое
        // соединение.
        let budget = isLast ? Self.quickLast : Self.quickAttempt
        DispatchQueue.main.asyncAfter(deadline: .now() + budget) { [weak self] in
            guard let self, self.qcGen == gen, self.vpnState != 2 else { return }
            if TunnelManager.available { self.tunnel.stop() } else { Core.stop() }
            self.tryNextQuick()
        }
    }

    /// Прекратить перебор (успех / стоп / ручной коннект): инвалидирует таймеры.
    func cancelQuick() { qcGen += 1; qcQueue = [] }

    func connect(_ host: Host, password: String = "") {
        let coord = coordinator
        withAnimation { connectedTo = host.id; vpnState = 1; expandedId = nil }
        let proto = host.proto, id = host.id
        let title = host.name.isEmpty ? host.id : host.name

        if TunnelManager.available {
            // Устройство: канал+туннель поднимает расширение; статус придёт
            // через applyTunnelStatus (NEVPNStatusDidChange). Успех гасит перебор
            // там (cancelQuick при .connected); фолбэк — по таймауту в tryNextQuick.
            Task {
                do {
                    // addRecent НЕ здесь: старт ≠ подключение. Добавит
                    // applyTunnelStatus, когда система сообщит .connected.
                    try await tunnel.start(coordinator: coord, hostId: id, password: password, proto: proto, title: title)
                } catch {
                    withAnimation { vpnState = 0; connectedTo = nil }
                    self.tryNextQuick()   // не поднялось — следующий кандидат
                }
            }
        } else {
            // Симулятор: NE недоступен — поднимаем только канал в процессе,
            // чтобы UI был живой (без реального туннеля).
            Task {
                let ok = await bg { Core.connect(coord, host: id, password: password, proto: proto) }
                withAnimation {
                    if ok { vpnState = 2; connectedSince = Date(); addRecent(id) }
                    else { vpnState = 0; connectedTo = nil }
                }
                if ok { self.cancelQuick() } else { self.tryNextQuick() }
            }
        }
    }

    func connectByCode(_ raw: String) {
        cancelQuick()   // ручной выбор перебивает авто-перебор «Старта»
        let code = raw.uppercased().trimmingCharacters(in: .whitespaces)
        guard !code.isEmpty else { return }
        if let h = hostById(code) { withAnimation { expandedId = code }; if !h.hasPassword && h.usable { connect(h) }; return }
        let coord = coordinator
        Task {
            if let h = await bg({ Core.resolve(coord, code: code) }) {
                resolvedHost = h
                if !h.hasPassword && h.usable { connect(h) } else { withAnimation { expandedId = h.id } }
            }
        }
    }

    func stop() {
        Haptics.tap()
        cancelQuick()   // пользователь остановил — гасим перебор кандидатов
        if TunnelManager.available {
            tunnel.stop()   // vpnState обнулит applyTunnelStatus по факту разрыва
        } else {
            Core.stop()
            withAnimation { connectedTo = nil; vpnState = 0; connectedSince = nil }
        }
    }

    func displayedHosts() -> [Host] {
        var out = hosts.filter { $0.id != hostCode || !hosting }  // прячем свой хост
        if let r = resolvedHost, !out.contains(where: { $0.id == r.id }) { out.insert(r, at: 0) }
        return out
    }

    // ── хост ──
    func ensureHostCode(_ done: (() -> Void)? = nil) {
        if !hostCode.isEmpty { done?(); return }
        let coord = coordinator
        Task {
            let (code, sig) = await bg { Core.newCode(coord) }
            if !code.isEmpty { setHostCode(code, sig) }
            done?()
        }
    }

    private func setHostCode(_ code: String, _ sig: String) {
        hostCode = code; hostSig = sig
        UserDefaults.standard.set(code, forKey: "host_code")
        UserDefaults.standard.set(sig, forKey: "host_sig")
    }

    func becomeHost() {
        persistHostSettings()
        withAnimation { starting = true }; hostError = nil
        ensureHostCode { [weak self] in
            guard let self else { return }
            let coord = self.coordinator
            let id = self.hostCode, token = self.hostToken, sig = self.hostSig
            let name = self.hostName, maxg = Int32(self.hostMax), pw = self.hostPassword
            let proto = self.hostProtocol, pub = self.hostPublic
            Task {
                let res = await bg { Core.hostStart(coord, id: id, token: token, codeSig: sig,
                                                     name: name, maxGuests: maxg, password: pw,
                                                     proto: proto, isPublic: pub) }
                withAnimation { self.starting = false }
                switch res {
                case "!NAT": self.hostError = "Нет публичного адреса (вы за NAT) — раздача отсюда невозможна"
                case "!SIG": self.setHostCode("", ""); self.hostError = "Код устарел, обновите и повторите"
                case "":     self.hostError = "Не удалось включить раздачу"
                default:     withAnimation { self.hosting = true }; self.hostStartedAt = Date(); self.startHostInfo()
                }
            }
        }
    }

    func stopHost() {
        Core.hostStop()
        hostInfoTask?.cancel()
        withAnimation { hosting = false; starting = false; myHostInfo = nil; hostStartedAt = nil }
    }

    /// Периодически тянем свою запись из каталога (гости/IP) — как на Android.
    private func startHostInfo() {
        hostInfoTask?.cancel()
        let coord = coordinator, code = hostCode
        hostInfoTask = Task {
            while !Task.isCancelled {
                let h = await bg { Core.resolve(coord, code: code) }
                if Task.isCancelled { break }
                myHostInfo = h
                try? await Task.sleep(nanoseconds: 3_000_000_000)
            }
        }
    }

    // Применить сразу (чипы протокола/видимости — один тап, флуда нет).
    func applyHostNow() {
        persistHostSettings()
        guard hosting else { return }
        let name = hostName, maxg = Int32(hostMax), pw = hostPassword, proto = hostProtocol, pub = hostPublic
        DispatchQueue.global().async { bmv_host_update(name, maxg, pw, proto, pub) }
    }

    // Применить с задержкой (имя/пароль при печати) — чтобы не дёргать сервер на
    // каждую букву. Слайдер лимита применяется отдельно, только по отпусканию.
    func applyHostDebounced() {
        persistHostSettings()
        guard hosting else { return }
        applyTask?.cancel()
        applyTask = Task {
            try? await Task.sleep(nanoseconds: 800_000_000)
            if Task.isCancelled { return }
            applyHostNow()
        }
    }

    func newHostCode() {
        let coord = coordinator
        let wasHosting = hosting
        if wasHosting { stopHost() }
        Task {
            let (code, sig) = await bg { Core.newCode(coord) }
            if !code.isEmpty { setHostCode(code, sig) }
            if wasHosting { becomeHost() }
        }
    }

    private func persistHostSettings() {
        let d = UserDefaults.standard
        d.set(hostName, forKey: "host_name"); d.set(hostMax, forKey: "host_max")
        d.set(hostPassword, forKey: "host_pw"); d.set(hostProtocol, forKey: "host_proto")
        d.set(hostPublic, forKey: "host_public")
    }

    // ── недавние ──
    private var recentKey: String { "recent::\(coordinator)" }
    private func loadRecent() { recent = UserDefaults.standard.stringArray(forKey: recentKey) ?? [] }
    private func addRecent(_ id: String) {
        var r = recent.filter { $0 != id }; r.insert(id, at: 0); r = Array(r.prefix(6))
        recent = r; UserDefaults.standard.set(r, forKey: recentKey)
    }
    func removeRecent(_ id: String) {
        recent = recent.filter { $0 != id }; UserDefaults.standard.set(recent, forKey: recentKey)
    }
}

// Флаг-эмодзи из 2-буквенного кода страны ("" если не код).
func flagEmoji(_ code: String) -> String {
    let c = code.uppercased()
    guard c.count == 2, c.unicodeScalars.allSatisfy({ $0.value >= 65 && $0.value <= 90 }) else { return "" }
    return String(c.unicodeScalars.compactMap { UnicodeScalar(127397 + $0.value).map(Character.init) })
}

// «5 мин», «1 ч 20 мин» из секунд.
/// Часы сессии — тикают посекундно: MM:SS, после часа H:MM:SS.
/// Именно посекундно: блок перерисовывается раз в секунду, и «12 мин»,
/// застывшее на минуту, выглядело бы зависшим.
func uptimeText(_ since: Date?) -> String {
    guard let since else { return "00:00" }
    let s = max(0, Int(Date().timeIntervalSince(since)))
    let h = s / 3600, m = (s % 3600) / 60, sec = s % 60
    return h > 0 ? String(format: "%d:%02d:%02d", h, m, sec) : String(format: "%02d:%02d", m, sec)
}
