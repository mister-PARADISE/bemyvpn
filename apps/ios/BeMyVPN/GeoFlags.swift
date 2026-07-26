import Foundation

/// Флаг страны по IP — определяется ЛОКАЛЬНО (не берётся из самоотчёта хоста).
/// Формат ip2cc.bin (распакованный BMV2): "BMV2" + n:u32 + deltas[n] + lens[n] + cc[n*2], BE.
enum GeoFlags {
    private static var starts: [UInt32] = []
    private static var ends: [UInt32] = []
    private static var cc: [UInt8] = []
    private static let lock = NSLock()
    private static var loaded = false

    static var ready: Bool { !starts.isEmpty }

    /// Загрузить базу (звать из фонового потока один раз).
    static func load() {
        lock.lock(); defer { lock.unlock() }
        if loaded { return }
        loaded = true
        guard let url = Bundle.main.url(forResource: "ip2cc", withExtension: "bin"),
              let data = try? Data(contentsOf: url), data.count > 8 else { return }
        data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            func u32(_ off: Int) -> UInt32 {
                (UInt32(raw[off]) << 24) | (UInt32(raw[off + 1]) << 16) | (UInt32(raw[off + 2]) << 8) | UInt32(raw[off + 3])
            }
            guard u32(0) == 0x424D5632 else { return }  // "BMV2"
            let n = Int(u32(4))
            guard n > 0, 8 + n * 8 + n * 2 <= raw.count else { return }
            var s = [UInt32](repeating: 0, count: n)
            var e = [UInt32](repeating: 0, count: n)
            var p = 8
            var deltas = [UInt32](repeating: 0, count: n)
            for i in 0..<n { deltas[i] = u32(p); p += 4 }
            var lens = [UInt32](repeating: 0, count: n)
            for i in 0..<n { lens[i] = u32(p); p += 4 }
            var prev: UInt32 = 0
            for i in 0..<n { s[i] = prev &+ deltas[i]; e[i] = s[i] &+ lens[i]; prev = e[i] }
            var c = [UInt8](repeating: 0, count: n * 2)
            for i in 0..<(n * 2) { c[i] = raw[p + i] }
            starts = s; ends = e; cc = c
        }
    }

    /// ISO-2 код страны по IPv4 (или nil).
    static func countryOf(_ ip: String) -> String? {
        if starts.isEmpty { return nil }
        guard let key = ipv4(ip) else { return nil }
        var lo = 0, hi = starts.count
        while lo < hi { let m = (lo + hi) >> 1; if starts[m] <= key { lo = m + 1 } else { hi = m } }
        let i = lo - 1
        guard i >= 0, key >= starts[i], key <= ends[i] else { return nil }
        return String(UnicodeScalar(cc[i * 2])) + String(UnicodeScalar(cc[i * 2 + 1]))
    }

    static func flagOfCc(_ code: String) -> String {
        let c = code.uppercased()
        guard c.count == 2, c.unicodeScalars.allSatisfy({ $0.value >= 65 && $0.value <= 90 }) else { return "🌍" }
        return String(c.unicodeScalars.compactMap { UnicodeScalar(127397 + $0.value).map(Character.init) })
    }

    private static func ipv4(_ ip: String) -> UInt32? {
        let parts = ip.split(separator: ".")
        guard parts.count == 4 else { return nil }
        var v: UInt32 = 0
        for p in parts { guard let o = UInt32(p), o <= 255 else { return nil }; v = (v << 8) | o }
        return v
    }
}
