import NetworkExtension
import Foundation
import Network
import BmvFFI

// ЖУРНАЛА ЗДЕСЬ НЕТ — И НЕ ДОЛЖНО БЫТЬ. Стоявшая тут временная диагностика
// (`os.Logger`, subsystem «org.bemyvpn.tunnel») писала адрес координатора и код
// хоста с `privacy: .public`, то есть открытым текстом в системный журнал
// устройства: он читается другими приложениями и уезжает в диагностические
// выгрузки. Ядро логгер не ставит СОЗНАТЕЛЬНО (см. шапку crates/bmv-ffi) —
// хост не хранит записей о трафике гостей и не может их выдать; расширение
// туннеля обязано жить по тому же правилу. Понадобится отладка — временный
// Logger только с `privacy: .private` на всём, что указывает на человека или
// его сеть, и снимать сразу после починки.

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
        guard !coordinator.isEmpty, !hostId.isEmpty else {
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
            // IPv6 ЗАБИРАЕМ СЕБЕ И ГЛУШИМ — зеркало `RouteGuard` на десктопе.
            //
            // Туннель несёт только IPv4. Пока здесь не было ни одного v6-маршрута,
            // весь IPv6-трафик шёл МИМО туннеля с настоящим адресом человека, а на
            // экране стояло «Защищено». На dual-stack сайтах это почти весь трафик:
            // клиенты предпочитают v6. Утечка тихая — сам он её не заметит никогда.
            //
            // Забрать ::/0 себе — единственный доступный приём: у NE нет «блокировать
            // семейство», а NEIPv6Settings без адреса даже не создать (адрес —
            // обязательный аргумент конструктора, в отличие от Android). Пакеты,
            // попавшие в utun, добьёт ядро: `bmv_tunnel::to_host_allowed` v6 хосту не
            // отдаёт, иначе настоящие адреса увидел бы ровно тот, от кого их прячут.
            //
            // Адрес НАРОЧНО из ULA (fd00::/8), а не глобальный: по таблице RFC 6724
            // у ULA-источника приоритет 3 против 35 у IPv4, поэтому система сама
            // предпочитает IPv4, и до чёрной дыры дело почти не доходит. С глобальным
            // адресом каждое соединение сперва висело бы на v6 до таймаута.
            //
            // Android этого НЕ ДЕЛАЕТ и делать не должен: там система сама кладёт в
            // таблицу VPN `unreachable default`, если мы v6 не объявили (см. комментарий
            // в BmvVpnService.kt). Здесь такого нет — либо забираем, либо утекает.
            //
            // ЧЕГО ЗДЕСЬ ПОКА НЕТ: настройка `guest.ipv6 = "allow"` (десктопная отдушина
            // для сетей, где v6 — единственный транспорт) до расширения не доезжает,
            // блокировка жёсткая. Провайдер настроек — providerConfiguration.
            let ipv6 = NEIPv6Settings(addresses: ["fd00:7::2"], networkPrefixLengths: [128])
            ipv6.includedRoutes = [NEIPv6Route.default()]
            settings.ipv6Settings = ipv6
            settings.mtu = 1400
            settings.dnsSettings = NEDNSSettings(servers: ["8.8.8.8"])

            self.setTunnelNetworkSettings(settings) { error in
                if let error = error {
                    completionHandler(error); return
                }
                guard let tunFd = self.utunFileDescriptor() else {
                    completionHandler(TunnelError.noTunFd); return
                }
                // dup: ядро берёт fd во владение и закроет его на остановке;
                // оригинал остаётся за системой NE, чтобы туннель жил.
                let owned = dup(tunFd)
                // utun=true — ядро снимает/добавляет 4-байтовый заголовок пакета.
                let started = bmv_start_tunnel(owned, true)
                if !started { close(owned) }
                if started { self.startPathMonitor(); self.startCoreWatchdog() }
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

    /// Домен ошибок, которыми объясняем приложению САМОСТОЯТЕЛЬНЫЙ конец сеанса.
    /// БЛИЗНЕЦ живёт в TunnelManager.swift (см. `stopDomain`): расширение и
    /// приложение — разные цели сборки, общего файла у них нет.
    private static let stopDomain = "org.bemyvpn.stop"

    /// Сторож ядра: сеанс кончился САМ — гасим системный туннель и говорим ПОЧЕМУ.
    ///
    /// Без него iOS продолжала считать VPN подключённым после конца сеанса: ядро
    /// уже сдалось, а utun остался на месте — весь трафик уходил в мёртвый канал.
    /// На экране при этом висело «Подключено». Тот же сторож давно есть на
    /// Android (BmvVpnService.startMonitor), здесь его просто не было.
    ///
    /// Причину кладём в ошибку `cancelTunnelWithError`: приложение прочитает её
    /// через `fetchLastDisconnectError`. Другого пути нет — к моменту, когда
    /// приложение заметит разрыв, этого процесса уже не существует.
    private func startCoreWatchdog() {
        DispatchQueue.global(qos: .utility).async { [weak self] in
            while true {
                Thread.sleep(forTimeInterval: 1)
                guard let self else { return }
                guard bmv_vpn_status() == 3 else { continue }
                // 1 — хост завершил раздачу (ошибки НЕТ), иначе связь потеряна.
                let hostLeft = bmv_stop_reason() == 1
                self.cancelTunnelWithError(NSError(
                    domain: Self.stopDomain,
                    code: hostLeft ? 1 : 2,
                    userInfo: [NSLocalizedDescriptionKey: hostLeft ? "Хост завершил раздачу — выберите другой" : "Связь с хостом пропала — подключитесь заново"]
                ))
                return
            }
        }
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
