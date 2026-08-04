import Foundation
import BmvFFI

/// Карточка хоста из каталога (совпадает с JSON, что отдаёт мост).
///
/// Правила показа посчитаны В МОМЕНТ РАЗБОРА, а не при отрисовке: экран берёт
/// готовое поле и мост в теле перерисовки не зовёт. Раньше подпись собирали на
/// экране, а число для цвета выдирали из этой же подписи обратно — верный
/// признак того, что граница проведена не там.
struct Host: Identifiable, Decodable, Equatable {
    let id: String
    let name: String
    let ip: String
    let country: String
    let guests: Int
    let max: Int
    let hasPassword: Bool
    let online: Bool
    let proto: String
    /// Адреса хоста через запятую — для пробы отклика ДО подключения.
    /// Необязательное: старые сборки ядра это поле не отдавали.
    let endpoints: String?

    // ── посчитано при разборе ──
    /// Имя протокола по-человечески («Обычный» / «Маскировка» / …).
    let protoName: String
    /// Уровень защиты — варианты view::Protection; картинку выбирает экран.
    let protection: Int
    /// Годен для подключения (живой и есть место) — на этом гасится кнопка.
    let usable: Bool

    enum CodingKeys: String, CodingKey {
        case id, name, ip, country, guests, max, hasPassword, online, endpoints
        case proto = "protocol"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        ip = try c.decode(String.self, forKey: .ip)
        country = try c.decode(String.self, forKey: .country)
        guests = try c.decode(Int.self, forKey: .guests)
        max = try c.decode(Int.self, forKey: .max)
        hasPassword = try c.decode(Bool.self, forKey: .hasPassword)
        online = try c.decode(Bool.self, forKey: .online)
        proto = try c.decode(String.self, forKey: .proto)
        endpoints = try c.decodeIfPresent(String.self, forKey: .endpoints)
        protoName = displayProtoName(proto)
        protection = Core.protection(proto)
        usable = bmv_host_usable(online, UInt32(Swift.max(0, guests)), UInt32(Swift.max(0, max)))
    }
}

/// Переходник к `protoName` из ContentView.swift.
///
/// Вызвать его из `Host.init` напрямую нельзя: там уже есть поле с этим именем.
/// А ПЕРЕНЕСТИ саму функцию сюда сегодня нечем — слова протокола отдаёт
/// `bmv_common::view::proto_name`, а двери `bmv_proto_name` в мосте нет
/// (см. bmv_ffi.h): вторая копия слов в этом файле была бы ровно тем
/// расхождением, которое ловит `one_place_per_rule`.
private func displayProtoName(_ p: String) -> String { protoName(p) }

/// Отклик до хоста: подпись и уровень тревоги ВМЕСТЕ.
///
/// Именно вместе, потому что порознь они и разъезжались: подпись брали из одного
/// места, цвет считали в другом — из этой же подписи, разобрав её обратно в
/// число. Оба поля приходят из справочника через мост.
struct Ping: Equatable {
    let text: String
    /// Варианты view::Alarm; цвет выбирает экран.
    let alarm: Int

    /// Первый замер ещё идёт: у экрана на это своя анимация ожидания.
    static let measuring = Ping(text: "…", alarm: 3)
}

/// Тонкая обёртка над C-мостом ядра (bmv_ffi). Логики тут нет — только типы и JSON.
enum Core {
    static let defaultCoordinator = "https://bemyvpn.net"

    /// Забрать строку из моста и ОСВОБОДИТЬ её (иначе утечка).
    private static func take(_ p: UnsafeMutablePointer<CChar>?) -> String {
        guard let p = p else { return "" }
        defer { bmv_free_string(p) }
        return String(cString: p)
    }

    // ── правила показа (общий справочник bmv_common::view) ──
    // Чистые функции: ни сети, ни блокировок — можно звать с главного потока.

    /// Уровень защиты по имени протокола — варианты view::Protection.
    static func protection(_ proto: String) -> Int { Int(bmv_protection(proto)) }

    /// Часы сеанса от отметки начала (nil — сеанса не было).
    static func sessionClock(since: Date?) -> String {
        let sec = since.map { Swift.max(0, Int($0.distance(to: Date()))) } ?? 0
        return take(bmv_session_clock(UInt64(sec)))
    }

    /// Замер отклика: nil — хост не ответил (мост подписывает это прочерком).
    static func ping(_ ms: Int?) -> Ping {
        let v = Int32(ms ?? -1)
        return Ping(text: take(bmv_ping_text(v)), alarm: Int(bmv_ping_alarm(v)))
    }

    /// Набранный человеком адрес → пригодный для работы. ПУСТАЯ СТРОКА — ОТКАЗ.
    static func coordinatorUrl(_ input: String) -> String { take(bmv_coordinator_url(input)) }

    /// Состояние VPN числом — варианты view::Vpn (склейка двух чисел моста).
    static func vpnKind(status: Int32, stopReason: Int32, wasConnected: Bool) -> Int {
        Int(bmv_vpn_kind(status, stopReason, wasConnected))
    }

    /// Подпись состояния VPN. Аргументы те же, что у `vpnKind`.
    static func vpnText(status: Int32, stopReason: Int32, wasConnected: Bool) -> String {
        take(bmv_vpn_text(status, stopReason, wasConnected))
    }

    // ── сигналинг ──
    static func health(_ coord: String) -> Bool { bmv_health(coord) }
    /// Круг до координатора в мс; 0 — ещё не мерили или связи нет.
    static func rttMs(_ coord: String) -> Int { Int(bmv_rtt_ms(coord)) }
    static func myIp(_ coord: String) -> String { take(bmv_my_ip(coord)) }

    /// Отклик до хоста в мс, nil = не ответил. Сессию на хосте не создаёт,
    /// поэтому звать можно по раскрытию карточки.
    static func probeRtt(_ coord: String, id: String, endpoints: String) -> Int? {
        let ms = bmv_probe_rtt(coord, id, endpoints)
        return ms >= 0 ? Int(ms) : nil
    }

    /// Новый код от сервера: "CODE|SIG" → (code, sig).
    static func newCode(_ coord: String) -> (code: String, sig: String) {
        let raw = take(bmv_new_code(coord))
        let parts = raw.split(separator: "|", maxSplits: 1).map(String.init)
        return (parts.first ?? "", parts.count > 1 ? parts[1] : "")
    }

    struct Envelope: Decodable { let version: UInt64; let hosts: [Host] }

    static func listWatch(_ coord: String, since: UInt64) -> (version: UInt64, hosts: [Host]) {
        let json = take(bmv_list_watch(coord, since))
        guard let data = json.data(using: .utf8),
              let env = try? JSONDecoder().decode(Envelope.self, from: data)
        else { return (0, []) }
        return (env.version, env.hosts)
    }

    static func resolve(_ coord: String, code: String) -> Host? {
        let json = take(bmv_resolve(coord, code))
        guard !json.isEmpty, let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(Host.self, from: data)
    }

    // ── гость ──
    @discardableResult
    static func connect(_ coord: String, host: String, password: String, proto: String) -> Bool {
        bmv_connect(coord, host, password, proto)
    }
    /// СЫРОЙ статус ядра (0..3). В состояние для показа его переводит `vpnKind`.
    static func vpnStatus() -> Int32 { bmv_vpn_status() }
    /// Почему сеанс кончился САМ: 0 — не кончался, ненулевое — кончился сам.
    /// ЧТО именно случилось, решают `vpnKind`/`vpnText`.
    static func stopReason() -> Int32 { bmv_stop_reason() }
    static func stop() { bmv_stop() }

    // ── хост ──
    static func hostStart(_ coord: String, id: String, token: String, codeSig: String,
                          name: String, maxGuests: Int32, password: String,
                          proto: String, isPublic: Bool) -> String {
        take(bmv_host_start(coord, id, token, codeSig, name, maxGuests, password, proto, isPublic))
    }
    static func hostStop() { bmv_host_stop() }
}

/// Выполнить блокирующий вызов моста в фоне (мост нельзя звать из главного потока).
func bg<T>(_ work: @escaping () -> T) async -> T {
    await withCheckedContinuation { cont in
        DispatchQueue.global(qos: .userInitiated).async { cont.resume(returning: work()) }
    }
}
