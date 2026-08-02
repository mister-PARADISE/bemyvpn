@preconcurrency import NetworkExtension

/// Управляет системным VPN-профилем и запускает расширение-туннель.
/// Приложение НЕ качает трафик само — это делает отдельный процесс-расширение
/// (PacketTunnelProvider). Здесь только: настроить профиль, стартовать/стопнуть,
/// слушать статус системы и отдавать его в UI.
@MainActor
final class TunnelManager {
    /// Network Extension не работает в симуляторе — там остаётся in-process
    /// поведение (канал без туннеля), чтобы UI можно было смотреть.
    static var available: Bool {
        #if targetEnvironment(simulator)
        return false
        #else
        return true
        #endif
    }

    private let bundleId = "org.bemyvpn.app.tunnel"
    private var manager: NETunnelProviderManager?
    private var observer: NSObjectProtocol?

    /// Домен ошибок, которыми расширение объясняет САМОСТОЯТЕЛЬНЫЙ конец сеанса.
    /// БЛИЗНЕЦ этой строки живёт в PacketTunnelProvider.swift — расширение и
    /// приложение это разные цели сборки, общего файла у них нет. Меняешь здесь —
    /// меняй и там, иначе объяснение молча перестанет доходить.
    static let stopDomain = "org.bemyvpn.stop"

    /// Чем кончился прошлый сеанс, по словам расширения (nil — оно ничего не
    /// сказало: значит выключили мы сами). Работает только на устройстве.
    func lastDisconnectError() async -> Error? {
        guard let conn = manager?.connection else { return nil }
        return await withCheckedContinuation { cont in
            conn.fetchLastDisconnectError { cont.resume(returning: $0) }
        }
    }

    /// Зовётся при смене статуса системного VPN (main-поток).
    var onStatus: ((NEVPNStatus) -> Void)?

    /// Подхватить уже существующий профиль на старте приложения (вдруг туннель
    /// ещё жив после перезапуска UI) и начать слушать статус.
    func prime() async {
        guard let mgr = try? await NETunnelProviderManager.loadAllFromPreferences().first else { return }
        manager = mgr
        observe(mgr)
        onStatus?(mgr.connection.status)
    }

    func start(coordinator: String, hostId: String, password: String, proto: String, title: String) async throws {
        let all = try await NETunnelProviderManager.loadAllFromPreferences()
        let mgr = all.first ?? NETunnelProviderManager()

        let p = NETunnelProviderProtocol()
        p.providerBundleIdentifier = bundleId
        p.serverAddress = title              // показывается в Настройках → VPN
        p.providerConfiguration = [
            "coordinator": coordinator, "hostId": hostId,
            "password": password, "proto": proto,
        ]
        mgr.protocolConfiguration = p
        mgr.localizedDescription = "BeMyVPN"
        mgr.isEnabled = true

        try await mgr.saveToPreferences()
        try await mgr.loadFromPreferences()  // перечитать после save — кварк NE
        observe(mgr)
        manager = mgr
        try mgr.connection.startVPNTunnel()
    }

    func stop() {
        // СНАЧАЛА просим расширение попрощаться с хостом (BYE), ПОКА туннель жив —
        // иначе на stopTunnel сокет уже мёртв и BYE не уходит (хост ждёт таймаут).
        // Затем реально останавливаем. Если сообщение не прошло — всё равно стоп.
        let conn = manager?.connection
        if let session = conn as? NETunnelProviderSession,
           session.status == .connected || session.status == .reasserting,
           let msg = "bye".data(using: .utf8),
           (try? session.sendProviderMessage(msg) { _ in conn?.stopVPNTunnel() }) != nil {
            // stopVPNTunnel вызовется в колбэке — после отправки BYE.
        } else {
            conn?.stopVPNTunnel()
        }
    }

    private func observe(_ mgr: NETunnelProviderManager) {
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = NotificationCenter.default.addObserver(
            forName: .NEVPNStatusDidChange, object: mgr.connection, queue: .main
        ) { [weak self] _ in
            let status = mgr.connection.status
            Task { @MainActor in self?.onStatus?(status) }
        }
    }
}
