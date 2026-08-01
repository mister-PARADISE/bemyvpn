import Foundation
import BmvFFI

/// Карточка хоста из каталога (совпадает с JSON, что отдаёт мост).
struct Host: Identifiable, Codable, Equatable {
    let id: String
    let name: String
    let ip: String
    let country: String
    let guests: Int
    let max: Int
    let hasPassword: Bool
    let online: Bool
    let isPublic: Bool
    let proto: String
    /// Адреса хоста через запятую — для пробы отклика ДО подключения.
    /// Необязательное: старые сборки ядра это поле не отдавали.
    let endpoints: String?

    enum CodingKeys: String, CodingKey {
        case id, name, ip, country, guests, max, hasPassword, online, endpoints
        case isPublic = "public"
        case proto = "protocol"
    }

    var usable: Bool { online && guests < max }
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

    struct Envelope: Codable { let version: UInt64; let hosts: [Host] }

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
    static func vpnStatus() -> Int32 { bmv_vpn_status() }
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
