import NetworkExtension
import Foundation
import Network
import os
import BmvFFI

// ДИАГНОСТИКА (временно, для отладки Mac Catalyst): смотреть в Console.app,
// фильтр по subsystem «org.bemyvpn.tunnel». Убрать после починки.
private let tlog = Logger(subsystem: "org.bemyvpn.tunnel", category: "tunnel")

/// Расширение Packet Tunnel — отдельный процесс, который iOS поднимает при
/// старте VPN. Здесь живёт РЕАЛЬНЫЙ туннель: фаза 1 (пробитие NAT + Noise) уже
/// в ядре, дальше система даёт utun-интерфейс, а ядро качает через него трафик.
///
/// Параметры (координатор/код/пароль/протокол) приходят из приложения через
/// providerConfiguration — App Group не нужен.
class PacketTunnelProvider: NEPacketTunnelProvider {

    enum TunnelError: Error { case badConfig, connectFailed, noTunFd }

    /// Следит за сменой сети (WiFi↔сотовая, смена вышки). При смене — форсирует
    /// реконнект туннеля в ядре, НЕ роняя utun (машина/метро: VPN сам оживает).
    private let pathMonitor = NWPathMonitor()
    private var lastPathKey = ""

    override func startTunnel(options: [String: NSObject]?, completionHandler: @escaping (Error?) -> Void) {
        let conf = (protocolConfiguration as? NETunnelProviderProtocol)?.providerConfiguration ?? [:]
        let coordinator = conf["coordinator"] as? String ?? ""
        let hostId      = conf["hostId"] as? String ?? ""
        let password    = conf["password"] as? String ?? ""
        let proto       = conf["proto"] as? String ?? ""
        tlog.notice("startTunnel: coord=\(coordinator, privacy: .public) host=\(hostId, privacy: .public) proto=\(proto, privacy: .public)")
        guard !coordinator.isEmpty, !hostId.isEmpty else {
            tlog.error("badConfig: пустой coordinator/host")
            completionHandler(TunnelError.badConfig); return
        }

        // Всё блокирующее — вне главного потока расширения.
        DispatchQueue.global(qos: .userInitiated).async {
            // ФАЗА 1: поднять канал к хосту (без туннеля). Ядро кладёт готовый
            // Link в PENDING_LINK, откуда его заберёт bmv_start_tunnel.
            let ok = coordinator.withCString { c in hostId.withCString { h in
                password.withCString { p in proto.withCString { pr in
                    bmv_connect(c, h, p, pr)
                }}}}
            tlog.notice("bmv_connect → \(ok ? "OK" : "FAIL", privacy: .public)")
            guard ok else { completionHandler(TunnelError.connectFailed); return }

            // Сеть туннеля — 1-в-1 как на Android (10.7.0.2/24, дефолт-маршрут,
            // MTU 1400, DNS 8.8.8.8): весь трафик заворачивается в туннель.
            // ВНИМАНИЕ: адрес продублирован в ядре константой GUEST_TUN_ADDR
            // (crates/bmv-tunnel/src/lib.rs) — по ней отсеиваются входящие пакеты,
            // адресованные не нам. Поменяешь здесь, не поменяв там — фильтр начнёт
            // резать ВСЁ и VPN тихо перестанет работать.
            let settings = NEPacketTunnelNetworkSettings(tunnelRemoteAddress: "10.7.0.1")
            let ipv4 = NEIPv4Settings(addresses: ["10.7.0.2"], subnetMasks: ["255.255.255.0"])
            ipv4.includedRoutes = [NEIPv4Route.default()]
            settings.ipv4Settings = ipv4
            settings.mtu = 1400
            settings.dnsSettings = NEDNSSettings(servers: ["8.8.8.8"])

            self.setTunnelNetworkSettings(settings) { error in
                if let error = error {
                    tlog.error("setTunnelNetworkSettings error: \(error.localizedDescription, privacy: .public)")
                    completionHandler(error); return
                }
                guard let tunFd = self.utunFileDescriptor() else {
                    tlog.error("utun fd НЕ найден")
                    completionHandler(TunnelError.noTunFd); return
                }
                tlog.notice("utun fd = \(tunFd, privacy: .public)")
                // dup: ядро берёт fd во владение и закроет его на остановке;
                // оригинал остаётся за системой NE, чтобы туннель жил.
                let owned = dup(tunFd)
                // utun=true — ядро снимает/добавляет 4-байтовый заголовок пакета.
                let started = bmv_start_tunnel(owned, true)
                tlog.notice("bmv_start_tunnel → \(started ? "OK" : "FAIL", privacy: .public)")
                if !started { close(owned) }
                if started { self.startPathMonitor() }
                completionHandler(started ? nil : TunnelError.connectFailed)
            }
        }
    }

    override func stopTunnel(with reason: NEProviderStopReason, completionHandler: @escaping () -> Void) {
        pathMonitor.cancel()
        DispatchQueue.global(qos: .userInitiated).async {
            bmv_stop()
            completionHandler()
        }
    }

    /// Сообщение от приложения. "bye" = попрощаться с хостом СЕЙЧАС, пока туннель
    /// ещё жив (сокет рабочий). Приложение шлёт это ПЕРЕД stopVPNTunnel — так BYE
    /// гарантированно уходит, а не проваливается на teardown в stopTunnel.
    override func handleAppMessage(_ messageData: Data, completionHandler: ((Data?) -> Void)?) {
        if String(data: messageData, encoding: .utf8) == "bye" {
            bmv_send_bye()
        }
        completionHandler?(nil)
    }

    /// Наблюдение за сетью: при смене активного интерфейса дёргаем ядро на
    /// немедленный реконнект (не ждём keepalive-таймаут). Ловит уход из WiFi в
    /// сотовую и обратно; смену вышек той же сети добьёт keepalive ядра (8с).
    private func startPathMonitor() {
        pathMonitor.pathUpdateHandler = { [weak self] path in
            guard let self = self, path.status == .satisfied else { return }
            // Ключ активного пути: типы интерфейсов. Меняется при WiFi↔сотовая.
            let key = path.availableInterfaces.map { "\($0.type)" }.joined(separator: ",")
            if self.lastPathKey.isEmpty { self.lastPathKey = key; return } // старт — без реконнекта
            guard key != self.lastPathKey else { return }
            self.lastPathKey = key
            // Сеть сменилась → показываем «переподключение» и форсируем реконнект.
            self.reasserting = true
            bmv_nudge_reconnect()
            self.clearReassertWhenUp()
        }
        pathMonitor.start(queue: DispatchQueue.global(qos: .utility))
    }

    /// Снять флаг reasserting, когда ядро восстановит канал (или по таймауту).
    private func clearReassertWhenUp() {
        DispatchQueue.global(qos: .utility).async { [weak self] in
            for _ in 0..<40 { // до ~20с
                if bmv_vpn_status() == 2 { break }
                Thread.sleep(forTimeInterval: 0.5)
            }
            self?.reasserting = false
        }
    }

    /// Найти дескриптор utun-интерфейса, который система создала под этот туннель.
    /// Приём как в WireGuard: перебрать fd и спросить у сокета имя интерфейса
    /// через getsockopt(SYSPROTO_CONTROL, UTUN_OPT_IFNAME).
    private func utunFileDescriptor() -> Int32? {
        var buf = [CChar](repeating: 0, count: Int(IFNAMSIZ))
        for fd: Int32 in 0...1024 {
            var len = socklen_t(buf.count)
            let ret = getsockopt(fd, 2 /* SYSPROTO_CONTROL */, 2 /* UTUN_OPT_IFNAME */, &buf, &len)
            if ret == 0 && String(cString: buf).hasPrefix("utun") {
                return fd
            }
        }
        return nil
    }
}
