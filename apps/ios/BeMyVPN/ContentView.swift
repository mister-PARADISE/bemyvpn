import SwiftUI
import UIKit
import CoreImage.CIFilterBuiltins

/// Имя протокола по-человечески — без крипто-жаргона, одним словом.
func protoName(_ p: String) -> String {
    switch p {
    case "noise", "noise-aes": return "Обычный"
    case "noise-obfs": return "Маскировка"
    case "plain", "": return "Без шифра"
    default: return p
    }
}

/// Значок протокола — семейство ЩИТА, это уровень защиты трафика.
/// Семейства не пересекаются, иначе один значок читается двумя способами:
///   щит  — защита (протокол): есть / замаскирована / нет
///   глаз — видимость хоста в списке
///   замок — доступ (пароль)
/// `eye.slash` тут был ошибкой: он читается как «скрыт из списка», хотя
/// «Маскировка» — про маскировку трафика, а не про видимость.
func protoIcon(_ p: String) -> String {
    switch p {
    case "noise", "noise-aes": return "lock.shield.fill"   // защищено
    case "noise-obfs": return "theatermasks.fill"          // защищено и переодето
    case "plain", "": return "shield.slash.fill"           // защиты нет
    default: return "questionmark.circle"
    }
}

/// Значок + подпись одной строкой: Text умеет вклеить SF Symbol внутрь себя.
func symbolText(_ icon: String, _ s: String) -> Text {
    Text(Image(systemName: icon)) + Text(" " + s)
}

/// Ненавязчивое пояснение к выбранному протоколу.
func protoDesc(_ p: String) -> String {
    switch p {
    case "noise", "noise-aes": return "Надёжное шифрование. Подходит почти всем — оставьте, если не уверены."
    case "noise-obfs": return "Прячет сам факт VPN: провайдер видит просто случайные данные. Чуть медленнее."
    case "plain", "": return "Шифрования нет — провайдер видит весь трафик. Только для сети, которой доверяете."
    default: return ""
    }
}

/// Страна с флагом — определяется по IP локально (GeoFlags), как на Android.
func countryLabel(_ h: Host) -> String {
    if let cc = GeoFlags.countryOf(h.ip) { return "\(GeoFlags.flagOfCc(cc)) \(cc)" }
    if !h.ip.isEmpty { return "🌍 \(h.ip)" }
    return h.country.isEmpty ? "—" : h.country
}

/// QR-картинка из кода (deep-link bemyvpn://CODE — ловит встроенный сканер).
func qrImage(_ code: String) -> UIImage? {
    let filter = CIFilter.qrCodeGenerator()
    filter.message = Data("bemyvpn://\(code)".utf8)
    filter.correctionLevel = "M"
    guard let out = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 12, y: 12)) else { return nil }
    let ctx = CIContext()
    guard let cg = ctx.createCGImage(out, from: out.extent) else { return nil }
    return UIImage(cgImage: cg)
}

struct ContentView: View {
    @EnvironmentObject var app: AppState
    var body: some View {
        ZStack(alignment: .bottom) {
            Theme.bg.ignoresSafeArea()
            Group {
                switch app.tab {
                case .server: ServerTab()
                case .vpn:    VPNTab()
                case .host:   HostTab()
                }
            }
            NavBar()
        }
        // Пробиваем нижнюю safe-area на КОРНЕ: только так нав-бар доходит до
        // физического низа. Если ignoresSafeArea вешать на сам бар внутри ZStack,
        // контейнер всё равно держит его на границе safe-area — отсюда «высоко».
        .ignoresSafeArea(.container, edges: .bottom)
    }
}

// ── Нижний нав-бар ────────────────────────────────────────────────────────────

struct NavBar: View {
    @EnvironmentObject var app: AppState

    var body: some View {
        HStack(spacing: 4) {
            serverCell
            vpnCell
            hostCell
        }
        .padding(6)
        .background(
            RoundedRectangle(cornerRadius: 28, style: .continuous)
                .fill(Color(hex: 0x161C2B).opacity(0.98))
                .overlay(RoundedRectangle(cornerRadius: 28, style: .continuous).stroke(Color.white.opacity(0.06), lineWidth: 1))
        )
        // Тень на почти-чёрном фоне: чёрная тень СЛИВАЕТСЯ с фоном (#0B0E14) и
        // не видна. Поэтому «подъём» бара делаем СВЕТЛЫМ ореолом сверху (тонкая
        // белёсая тень вверх) + плотной чёрной снизу под баром.
        .shadow(color: .black.opacity(0.65), radius: 5, x: 0, y: 2)
        .shadow(color: .black.opacity(0.8), radius: 22, x: 0, y: 12)
        .shadow(color: .white.opacity(0.10), radius: 14, x: 0, y: -6)
        .padding(.horizontal, 18)
        // safe-area пробита на корне (ContentView), поэтому зазор снизу задаём
        // руками. Нужно ОДНОВРЕМЕННО: (1) не влезать в зону home-indicator внизу
        // и (2) не «висеть» высоко над краем. 34pt — бар заметно приподнят над
        // краем, но не отрывается. Выверено на симуляторе.
        .padding(.bottom, 34)
    }

    /// Радиус внутренней таблетки. Концентричность: внешний R − отступ.
    /// 28 − 6 = 22; иначе внутренняя форма не следует внешней.
    private let innerR: CGFloat = 22

    /// Высота коробки значка. ОБЯЗАНА быть фиксированной: `Image(systemName:)`
    /// подгоняется под рамку конкретного символа, а она у bolt/xmark/shield
    /// разная — от этого при переключении на VPN бар менял высоту. У эмодзи
    /// метрики были одинаковые, поэтому раньше не всплывало.
    private let iconBox: CGFloat = 22

    /// Ячейка-переход. `live` — точка состояния: горит, только когда ячейка
    /// ведёт на ДРУГУЮ вкладку. На своей состояние и так написано словом.
    private func cell(_ t: Tab, icon: String, label: String, live: Color? = nil) -> some View {
        let active = app.tab == t
        return VStack(spacing: 3) {
            Image(systemName: icon).font(.system(size: 19, weight: .semibold))
                .frame(height: iconBox)
                .foregroundColor(active ? Theme.accent : Theme.dim)
            // Одна строка и ужимание: подпись у ячейки МЕНЯЕТСЯ («Хост» →
            // «Раздать»), и без этого длинная могла бы распереть свою треть,
            // сдвинув границы соседних — бар «скачет» при переключении вкладок.
            Text(label).font(.system(size: 11, weight: .bold)).foregroundColor(active ? Theme.accent : Theme.dim)
                .lineLimit(1).minimumScaleFactor(0.8)
        }
        .frame(maxWidth: .infinity).padding(.vertical, 8)
        .background(active ? RoundedRectangle(cornerRadius: innerR, style: .continuous).fill(Theme.accent.opacity(0.16)) : nil)
        .overlay(alignment: .top) {
            if let live {
                Circle().fill(live).frame(width: 7, height: 7)
                    .overlay(Circle().stroke(Color(hex: 0x161C2B), lineWidth: 2))
                    .offset(x: 19, y: 3)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { withAnimation(.easeInOut(duration: 0.18)) { app.tab = t } }
    }

    /// Ячейка-включатель. Подкраска и рамка в цвет состояния — раньше здесь был
    /// сплошной градиент во всю яркость, и он перетягивал на себя весь экран.
    private func action(_ icon: String, _ label: String, hue: Color, _ tap: @escaping () -> Void) -> some View {
        VStack(spacing: 3) {
            Image(systemName: icon).font(.system(size: 19, weight: .bold))
                .frame(height: iconBox)
                .foregroundColor(hue)
            Text(label).font(.system(size: 11, weight: .bold)).foregroundColor(hue)
                .lineLimit(1).minimumScaleFactor(0.8)
        }
        .frame(maxWidth: .infinity).padding(.vertical, 8)
        .background(
            RoundedRectangle(cornerRadius: innerR, style: .continuous).fill(hue.opacity(0.14))
                .overlay(RoundedRectangle(cornerRadius: innerR, style: .continuous).stroke(hue.opacity(0.37), lineWidth: 1))
        )
        .contentShape(Rectangle())
        .onTapGesture(perform: tap)
    }

    // «Сервер» — навигация всегда: у вкладки нет своего включателя. Зато на ней
    // горит состояние связи, видное С ЛЮБОЙ вкладки. Когда всё хорошо — точки
    // нет: молчание и есть «ок». Раньше об обрыве кричала полоса на вкладке VPN.
    private var serverCell: some View {
        cell(.server, icon: "server.rack", label: "Сервер",
             live: app.serverOnline == true ? nil : (app.serverOnline == false ? Theme.red : Theme.amber))
    }

    @ViewBuilder private var vpnCell: some View {
        if app.tab != .vpn {
            cell(.vpn, icon: "shield.fill", label: "VPN",
                 live: app.vpnState == 0 ? nil : (app.vpnState == 2 ? Theme.green : Theme.amber))
        } else if app.vpnState == 0 {
            action("bolt.fill", "Старт", hue: Theme.green) { app.quickConnect() }
        } else {
            action("xmark", app.vpnState == 1 ? "Отмена" : "Стоп", hue: Theme.red) { app.stop() }
        }
    }

    @ViewBuilder private var hostCell: some View {
        if app.tab != .host {
            cell(.host, icon: "wifi.router.fill", label: "Хост",
                 live: (app.hosting || app.starting) ? (app.hosting ? Theme.green : Theme.amber) : nil)
        } else if app.hosting || app.starting {
            action("xmark", app.starting ? "Отмена" : "Стоп", hue: Theme.red) { app.stopHost() }
        } else {
            action("power", "Раздать", hue: Theme.green) { app.becomeHost() }
        }
    }
}

// ── переиспользуемое ──────────────────────────────────────────────────────────

struct Dot: View {
    let color: Color
    var pulse = false
    @State private var on = false
    var body: some View {
        Circle().fill(color).frame(width: 11, height: 11)
            .opacity(pulse && on ? 0.35 : 1)
            .onAppear { if pulse { withAnimation(.easeInOut(duration: 1).repeatForever()) { on = true } } }
    }
}

/// Общая «шапка» плитки — подпись мелко сверху, значение снизу.
private struct TileBody<Trailing: View>: View {
    let label: String; let value: String
    var symbol: String? = nil
    var valueColor: Color = Theme.fg
    var mono = false
    @ViewBuilder let trailing: Trailing
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label).foregroundColor(Theme.dim).font(.system(size: 10, weight: .heavy)).kerning(0.7)
            // Значение и значок — ПО ЦЕНТРУ ячейки. Раньше `symbol` вставал слева
            // от текста, а `trailing` уезжал вправо за распоркой, и значки в
            // соседних плитках оказывались по разные стороны — взглядцеплялся за
            // разнобой. Заголовок остаётся слева: он подпись к ячейке, а не
            // содержимое.
            HStack(spacing: 5) {
                if let symbol {
                    Image(systemName: symbol).font(.system(size: 12, weight: .semibold)).foregroundColor(valueColor)
                }
                if !value.isEmpty {
                    Text(value).foregroundColor(valueColor)
                        .font(.system(size: 13.5, weight: .semibold, design: mono ? .monospaced : .default))
                        .lineLimit(1).minimumScaleFactor(0.6)
                }
                trailing
            }
            .frame(maxWidth: .infinity, alignment: .center)
        }
        .padding(.horizontal, 11).padding(.vertical, 8)
        // Пара «подпись + значение» центрируется в ячейке фиксированной высоты.
        // Раньше высоту задавали отступы, а сумма строк с их межстрочными
        // интервалами перекрывала её — значение выдавливало к нижнему краю, и
        // казалось, что оно проваливается.
        .frame(maxWidth: .infinity, minHeight: 52, alignment: .leading)
    }
}

/// Фон плитки: единый для всех — разница только в содержимом, не в оформлении.
private func tileBackground(_ accent: Color?) -> some View {
    RoundedRectangle(cornerRadius: 12, style: .continuous)
        .fill(Theme.tile)
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous)
            .stroke(accent ?? Color.white.opacity(0.07), lineWidth: 1))
}

/// Строка состояния для РАБОТАЮЩЕГО режима: значок кружком, название, часы.
///
/// Значок никуда не девается и в работе — он опознаёт экран с одного взгляда.
/// Но держать 84pt картинки там, где нужны код и цифры, расточительно: панель
/// прижата к верху и висит на экране постоянно.
struct StatusLine: View {
    let icon: String
    let title: String
    var clock: String? = nil
    let tint: Color
    var body: some View {
        HStack(spacing: 10) {
            ZStack {
                Circle().fill(tint.opacity(0.13))
                Circle().stroke(tint.opacity(0.3), lineWidth: 1)
                Image(systemName: icon).font(.system(size: 17, weight: .semibold)).foregroundColor(tint)
            }
            .frame(width: 38, height: 38)
            Text(title).foregroundColor(Theme.fg).font(.system(size: 17, weight: .heavy))
                .lineLimit(1).minimumScaleFactor(0.7)
            Spacer(minLength: 6)
            if let clock {
                Text(clock).foregroundColor(tint)
                    .font(.system(size: 14, weight: .bold, design: .monospaced))
            }
        }
    }
}

/// Кнопки «поделиться» — В НИЗУ панели состояния, прямо под кодом.
///
/// Код сети и QR — то, ради чего эту панель открывают. Оторванные от кода,
/// который копируют, они читались бы как отдельная штука неясно про что.
struct ShareButtons: View {
    let code: String
    @Binding var qrCode: String?
    @State private var copied = false
    var body: some View {
        HStack(spacing: 8) {
            button(copied ? "checkmark" : "doc.on.doc", copied ? "Скопировано" : "Скопировать", green: copied) {
                UIPasteboard.general.string = code
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                withAnimation(.easeOut(duration: 0.15)) { copied = true }
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.3) { withAnimation { copied = false } }
            }
            button("qrcode", "QR-код", green: false) { qrCode = code }
        }
    }
    private func button(_ icon: String, _ title: String, green: Bool, _ tap: @escaping () -> Void) -> some View {
        Button(action: tap) {
            HStack(spacing: 8) {
                Image(systemName: icon).font(.system(size: 15, weight: .bold))
                Text(title).font(.system(size: 14, weight: .bold)).lineLimit(1).minimumScaleFactor(0.7)
            }
            .foregroundColor(green ? Theme.green : Theme.accent)
            .frame(maxWidth: .infinity).frame(height: 48)
            .background(RoundedRectangle(cornerRadius: 15, style: .continuous).fill(Color(hex: 0x1B2333))
                .overlay(RoundedRectangle(cornerRadius: 15, style: .continuous)
                    .stroke(green ? Theme.green.opacity(0.5) : Theme.accent.opacity(0.24), lineWidth: 1)))
        }.buttonStyle(PressStyle())
    }
}

/// Негромкая кнопка внутри панели — для РЕДКИХ действий («Новый код»).
struct QuietButton: View {
    let icon: String
    let title: String
    var action: () -> Void
    @State private var did = false
    var body: some View {
        Button {
            action()
            withAnimation(.easeOut(duration: 0.15)) { did = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.3) { withAnimation { did = false } }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: did ? "checkmark" : icon).font(.system(size: 12, weight: .bold))
                Text(did ? "Готово" : title).font(.system(size: 12.5, weight: .bold))
            }
            .foregroundColor(did ? Theme.green : Theme.dim)
            .frame(maxWidth: .infinity).frame(height: 34)
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(did ? Theme.green.opacity(0.5) : Color.white.opacity(0.08), lineWidth: 1))
        }.buttonStyle(PressStyle())
    }
}

/// Тихий знак состояния у заголовка раздела — вместо полосы во всю ширину.
struct StateChip: View {
    let text: String
    var tint: Color = Theme.amber
    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(tint).frame(width: 6, height: 6)
            Text(text).foregroundColor(tint).font(.system(size: 11, weight: .bold))
        }
    }
}

/// Обёртка прижатой панели: она лежит ВНЕ ScrollView, поэтому статус не уезжает
/// при прокрутке, а содержимое уходит под неё.
struct PinnedPanel<Content: View>: View {
    let tint: Color
    @ViewBuilder let content: Content
    var body: some View {
        VStack(spacing: 8) { content }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16).padding(.horizontal, 18)
            .background(
                RoundedRectangle(cornerRadius: 22, style: .continuous)
                    .fill(LinearGradient(colors: [Theme.panel, Theme.card], startPoint: .top, endPoint: .bottom))
                    .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).stroke(tint.opacity(0.2), lineWidth: 1))
            )
            .padding(.horizontal, 20).padding(.top, 20).padding(.bottom, 12)
            .background(Theme.bg)
            .shadow(color: .black.opacity(0.55), radius: 12, y: 8)
            .zIndex(1)
    }
}

/// Крупный круг статуса — для состояний, где показывать больше нечего.
struct HeroCircle: View {
    let icon: String
    let tint: Color
    var pulsing = false
    var glowing = false
    @State private var wave = false
    var body: some View {
        ZStack {
            if pulsing {
                Circle().stroke(tint.opacity(0.5), lineWidth: 2).frame(width: 72, height: 72)
                    .scaleEffect(wave ? 1.28 : 1).opacity(wave ? 0 : 0.7)
            }
            Circle().fill(tint.opacity(0.13)).frame(width: 72, height: 72)
            Circle().stroke(tint.opacity(0.3), lineWidth: 1).frame(width: 72, height: 72)
            Image(systemName: icon).font(.system(size: 30, weight: .semibold)).foregroundColor(tint)
        }
        .frame(height: 74)
        .shadow(color: tint.opacity(glowing ? 0.32 : 0), radius: 16)
        .onAppear { withAnimation(.easeOut(duration: 1.2).repeatForever(autoreverses: false)) { wave = true } }
    }
}

/// Лёгкое «вдавливание» под пальцем — кнопка отвечает на нажатие.
struct PressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

/// Кнопка в карточке: значок + подпись, приподнятая поверхность, отклик на палец.
/// Если задан `copy`, кладёт его в буфер и на 1.3с превращается в «Скопировано ✓» —
/// без этого тап по «копировать» ощущается как несработавшая кнопка.
struct CardButton: View {
    let icon: String
    let title: String
    var copy: String? = nil
    var action: (() -> Void)? = nil
    @State private var copied = false

    var body: some View {
        Button {
            if let c = copy {
                guard !c.isEmpty else { return }
                UIPasteboard.general.string = c
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                withAnimation(.easeOut(duration: 0.15)) { copied = true }
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.3) { withAnimation { copied = false } }
            } else {
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
                action?()
            }
        } label: {
            HStack(spacing: 7) {
                Image(systemName: copied ? "checkmark" : icon).font(.system(size: 13, weight: .bold))
                    .foregroundColor(copied ? Theme.green : Theme.accent)
                Text(copied ? "Скопировано" : title).font(.system(size: 14, weight: .bold))
                    .foregroundColor(copied ? Theme.green : Theme.fg)
                    .lineLimit(1).minimumScaleFactor(0.8)
            }
            .frame(maxWidth: .infinity).padding(.vertical, 12)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(Theme.tile)
                    .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .stroke(copied ? Theme.green.opacity(0.5) : Color.white.opacity(0.08), lineWidth: 1))
            )
        }.buttonStyle(PressStyle())
    }
}

/// Крупная кнопка «скопировать» — вид тот же, что был, добавлено подтверждение.
struct BigCopyButton: View {
    let value: String
    @State private var copied = false
    var body: some View {
        Button {
            guard !value.isEmpty else { return }
            UIPasteboard.general.string = value
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            withAnimation(.easeOut(duration: 0.15)) { copied = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.4) { withAnimation { copied = false } }
        } label: {
            HStack(spacing: 8) {
                Image(systemName: copied ? "checkmark" : "doc.on.doc").font(.system(size: 15, weight: .bold))
                Text(copied ? "Скопировано" : "Скопировать код").fontWeight(.bold)
            }
            .foregroundColor(.white)
            .frame(maxWidth: .infinity).padding(.vertical, 15)
            .background(copied ? AnyShapeStyle(Theme.green)
                               : AnyShapeStyle(LinearGradient(colors: [Theme.accent, Theme.accent2],
                                                              startPoint: .leading, endPoint: .trailing)))
            .cornerRadius(14)
        }.buttonStyle(PressStyle())
    }
}

struct PingTile: View {
    let value: String
    private var waiting: Bool { value == "…" }
    private var noAnswer: Bool { value == "—" }
    @State private var spin = false

    /// Цвет по величине задержки — чтобы годность хоста читалась без чтения цифр.
    /// Пороги под VPN: до 60мс разницы не чувствуешь, после 150мс уже мешает.
    private var tint: Color {
        guard let ms = Int(value.split(separator: " ").first.map(String.init) ?? "") else { return Theme.fg }
        if ms < 60 { return Theme.green }
        if ms <= 150 { return Theme.amber }
        return Theme.red
    }

    var body: some View {
        Group {
            if waiting {
                TileBody(label: "ПИНГ", value: "", symbol: nil, valueColor: Theme.fg) {
                    Image(systemName: "arrow.triangle.2.circlepath")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundColor(Theme.accent)
                        .rotationEffect(.degrees(spin ? 360 : 0))
                        .animation(.linear(duration: 0.9).repeatForever(autoreverses: false), value: spin)
                        .onAppear { spin = true }
                }
                .background(tileBackground(nil))
            } else if noAnswer {
                // Только знак, без слова: перечёркнутая антенна говорит сама,
                // а «нет» рядом с ней — это то же самое ещё раз.
                StatTile(label: "ПИНГ", value: "", symbol: "antenna.radiowaves.left.and.right.slash", tint: Theme.dim)
            } else {
                StatTile(label: "ПИНГ", value: value, tint: tint)
            }
        }
        .animation(.easeOut(duration: 0.25), value: value)
    }
}

struct StatTile: View {
    let label: String; let value: String
    var symbol: String? = nil
    var tint: Color = Theme.fg
    var body: some View {
        TileBody(label: label, value: value, symbol: symbol, valueColor: tint) { EmptyView() }
            .background(tileBackground(nil))
    }
}

/// Плитка-кнопка: тап запускает действие (например, перепроверить пинг).
/// Иконка акцентом — тот же намёк «меня можно тыкнуть», что и у копирования,
/// но другая: круговая стрелка вместо листков.
struct ActionTile: View {
    let label: String; let value: String
    var tint: Color = Theme.fg
    let icon: String
    var busy = false
    let action: () -> Void
    var body: some View {
        Button {
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            action()
        } label: {
            TileBody(label: label, value: busy ? "проверяю…" : value, valueColor: busy ? Theme.dim : tint) {
                if busy { ProgressView().controlSize(.mini).tint(Theme.accent) }
                else { Image(systemName: icon).font(.system(size: 11, weight: .bold)).foregroundColor(Theme.accent) }
            }
            .background(tileBackground(nil))
        }.buttonStyle(PressStyle())
    }
}

/// Плитка со значением, которое копируется тапом (код, IP).
/// От обычной отличается только акцентной иконкой — не рамкой и не фоном,
/// иначе плитки в одном блоке выглядят разнородными.
struct CopyTile: View {
    let label: String; let value: String
    @State private var copied = false
    private var empty: Bool { value.isEmpty || value == "—" }
    var body: some View {
        Button {
            guard !empty else { return }
            UIPasteboard.general.string = value
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            withAnimation(.easeOut(duration: 0.15)) { copied = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.3) { withAnimation { copied = false } }
        } label: {
            TileBody(label: label, value: copied ? "Скопировано" : value,
                     valueColor: copied ? Theme.green : Theme.fg, mono: !copied) {
                if !empty {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .foregroundColor(copied ? Theme.green : Theme.accent).font(.system(size: 11, weight: .bold))
                }
            }
            .background(tileBackground(copied ? Theme.green.opacity(0.5) : nil))
        }.buttonStyle(.plain)
    }
}

struct Card<Content: View>: View {
    @ViewBuilder let content: Content
    var body: some View {
        VStack(alignment: .leading, spacing: 10) { content }
            .padding(16).frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.panel).cornerRadius(16)
    }
}

func sectionLabel(_ t: String) -> some View {
    Text(t).foregroundColor(Theme.dim).font(.system(size: 13, weight: .bold))
        .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 6)
}
func tabHeader(_ icon: String, _ title: String) -> some View {
    HStack(spacing: 10) {
        Image(systemName: icon).font(.system(size: 23, weight: .semibold)).foregroundColor(Theme.accent)
        Text(title).font(.system(size: 26, weight: .heavy)).foregroundColor(Theme.fg)
    }
}
extension View { func navPadding() -> some View { self.padding(.bottom, 96) } }
func gradientButton(_ title: String) -> some View {
    Text(title).fontWeight(.bold).foregroundColor(.white)
        .frame(maxWidth: .infinity).padding(.vertical, 15)
        .background(LinearGradient(colors: [Theme.accent, Theme.accent2], startPoint: .leading, endPoint: .trailing))
        .cornerRadius(14)
}
/// Спокойная главная кнопка: подкраска + рамка + цветной текст — тот же приём,
/// что у ShareButtons и у действующей ячейки нав-бара. Для действий ВНУТРИ
/// списка: карточка раскрывается прямо в нём, и сплошная синяя плашка во всю
/// ширину перетягивала внимание с самих цифр хоста, ради которых карточку и
/// раскрывают.
func calmButton(_ title: String) -> some View {
    Text(title).fontWeight(.bold).foregroundColor(Theme.accent)
        .frame(maxWidth: .infinity).padding(.vertical, 15)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Theme.accent.opacity(0.15))
                .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .stroke(Theme.accent.opacity(0.4), lineWidth: 1))
        )
}
/// Флаг для «аватарки» слева в списке (🌍 если страна не определилась).
func hostFlag(_ h: Host) -> String {
    GeoFlags.countryOf(h.ip).map { GeoFlags.flagOfCc($0) } ?? "🌍"
}

/// Подпись под именем. Флаг сюда НЕ кладём — он теперь аватарка слева.
///
/// В строке места мало и хвост уходит под многоточие, поэтому здесь остаётся
/// только то, ради чего на строку смотрят. СЫРОГО IP тут нет: он стоял первым
/// (когда страна не определилась) и один занимал всю ширину — до счётчика
/// гостей многоточие просто не доходило. Адрес виден в раскрытой карточке,
/// там под него отдельная плитка. Счётчик гостей есть ВСЕГДА — это и делает
/// подпись непустой при любых данных.
///
/// ЗНАЧОК ПРОТОКОЛА — У КАЖДОГО ПРОТОКОЛА И ПЕРВЫМ. Раньше он был только у
/// «Маскировки», и его отсутствие означало сразу две противоположные вещи:
/// «Обычный» (шифр есть) и «Без шифра» (шифра нет) — незащищённый хост
/// выглядел в списке ровно как защищённый. Строку это не перегружает: слов не
/// прибавилось, строка как была одна, так и осталась. Первым, а не в хвосте,
/// потому что хвост — то, что съедает многоточие: знак защиты пропадал бы
/// ровно там, где строке не хватило ширины.
func hostSubtitle(_ h: Host) -> Text {
    var parts: [String] = []
    if let cc = GeoFlags.countryOf(h.ip) { parts.append(cc) }
    // Потолок хост может и не объявить (0) — дробь «1/0» в этом случае врёт.
    parts.append(h.max > 0 ? "гостей \(h.guests)/\(h.max)" : "гостей \(h.guests)")
    return symbolText(protoIcon(h.proto), parts.joined(separator: " · "))
}

// ── ВКЛАДКА «СЕРВЕР» ─────────────────────────────────────────────────────────

struct ServerTab: View {
    @EnvironmentObject var app: AppState
    @State private var coordField = ""

    @State private var pulsing = false

    // Кольцо отвечает на «сервер доступен?» — это ДА/НЕТ. Качество связи
    // показывает пинг отдельной плиткой. Раньше кольцо краснело от медленного
    // пинга, и «На связи» с красной точкой читалось как поломка.
    private var tint: Color {
        switch app.serverOnline {
        case .some(true): return Theme.green
        case .some(false): return Theme.red
        default: return Theme.amber
        }
    }
    private var icon: String {
        app.serverOnline == false ? "antenna.radiowaves.left.and.right.slash" : "antenna.radiowaves.left.and.right"
    }
    private var statusText: String {
        switch app.serverOnline { case .some(true): return "На связи"; case .some(false): return "Нет связи — восстанавливаю…"; default: return "Проверяю связь…" }
    }
    private var pingColor: Color {
        app.ping < 200 ? Theme.green : (app.ping < 600 ? Theme.fg : Theme.amber)
    }
    private var addr: String { app.coordinator.replacingOccurrences(of: "https://", with: "").replacingOccurrences(of: "http://", with: "") }

    var body: some View {
        VStack(spacing: 0) {
            hero
            scrollBody
        }
        // Открыли вкладку — сразу свежая цифра, не дожидаясь очередного круга.
        // Сам цикл живёт всегда (см. watchServer), как на десктопе.
        .onAppear { coordField = addr; app.checkServer() }
    }

    private var scrollBody: some View {
        ScrollView(showsIndicators: false) {
            VStack(alignment: .leading, spacing: 14) {
                Text("Сервер ведёт каталог хостов и сводит участников. Ваш трафик через него не проходит.")
                    .foregroundColor(Theme.dim).font(.system(size: 12))
                    .frame(maxWidth: .infinity, alignment: .leading)

                sectionLabel("Другой адрес сервера")
                TextField("адрес сервера", text: $coordField)
                    .foregroundColor(Theme.fg).autocorrectionDisabled().textInputAutocapitalization(.never)
                    .padding(14).background(Theme.card).cornerRadius(14)

                Button { app.saveCoordinator(coordField.isEmpty ? app.coordinator : coordField) } label: { gradientButton("Сохранить и проверить") }
                if app.coordinator != Core.defaultCoordinator {
                    Button { coordField = Core.defaultCoordinator; app.saveCoordinator(Core.defaultCoordinator) } label: {
                        Text("Вернуть стандартный сервер").foregroundColor(Theme.dim).font(.system(size: 13)).frame(maxWidth: .infinity)
                    }
                }

                let hist = app.serverHistory.filter { $0 != app.coordinator }
                if !hist.isEmpty {
                    sectionLabel("Недавние серверы")
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(hist, id: \.self) { url in
                                Text(url.replacingOccurrences(of: "https://", with: "").replacingOccurrences(of: "http://", with: ""))
                                    .font(.system(size: 13, weight: .bold)).foregroundColor(Theme.fg)
                                    .padding(.horizontal, 14).padding(.vertical, 9).background(Theme.cardSel).cornerRadius(16)
                                    .onTapGesture { coordField = url; app.saveCoordinator(url) }
                            }
                        }.padding(.horizontal, 1)
                    }
                }
            }
            .padding(20).padding(.top, 2).navPadding()
        }
    }

    /// Прижатая панель. Когда связь есть, круг уступает место цифрам: смотреть
    /// на большой значок «всё хорошо» смысла нет.
    private var hero: some View {
        PinnedPanel(tint: tint) {
            if app.serverOnline == true {
                StatusLine(icon: icon, title: statusText, tint: tint)
                Text(addr).foregroundColor(Theme.dim).font(.system(size: 13, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                HStack(spacing: 8) {
                    // Обычная плитка, а не кнопка: проверка идёт сама каждые 3
                    // секунды, и нажатие экономило бы в лучшем случае их же.
                    StatTile(label: "ПИНГ", value: "\(app.ping) мс", tint: pingColor)
                    StatTile(label: "ХОСТОВ", value: "\(app.hosts.count)")
                }
                CopyTile(label: "ВАШ IP", value: app.myIp.isEmpty ? "—" : app.myIp)
            } else {
                HeroCircle(icon: icon, tint: tint, pulsing: app.checking)
                Text(statusText).foregroundColor(Theme.fg).font(.system(size: 21, weight: .heavy))
                    .multilineTextAlignment(.center)
                Text(addr).foregroundColor(Theme.dim).font(.system(size: 13, design: .monospaced))
            }
        }
        .animation(.easeInOut(duration: 0.3), value: app.serverOnline)
    }
}

// ── ВКЛАДКА «VPN» ────────────────────────────────────────────────────────────

struct VPNTab: View {
    @EnvironmentObject var app: AppState
    @State private var code = ""
    @State private var showScanner = false
    @State private var inviteCode: String? = nil

    private func handleScanned(_ s: String) {
        if let url = URL(string: s), url.scheme == "bemyvpn" { app.openDeepLink(url) }
        else { app.connectByCode(s) }
    }

    /// Чип недавнего хоста: маленький флаг слева, затем имя. Первый (последний,
    /// к кому подключались) — подсвечен акцентной рамкой.
    private func recentChip(_ id: String, highlighted: Bool) -> some View {
        let host = app.hostById(id)
        return HStack(spacing: 6) {
            Text(host.map { hostFlag($0) } ?? "🌐").font(.system(size: 15))
            Text(host?.name ?? id).font(.system(size: 13, weight: .bold))
                .foregroundColor(highlighted ? Theme.accent : Theme.fg).lineLimit(1)
        }
        .padding(.horizontal, 13).padding(.vertical, 9)
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(highlighted ? Theme.accent.opacity(0.16) : Theme.cardSel)
                .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(highlighted ? Theme.accent.opacity(0.5) : Color.clear, lineWidth: 1))
        )
        .contentShape(Rectangle())
        .onTapGesture { if app.vpnState == 0 { app.connectByCode(id) } }
    }

    var body: some View {
        VStack(spacing: 0) {
            // Панель ВНЕ прокрутки: статус не уезжает, содержимое уходит под неё.
            VPNHero(inviteCode: $inviteCode)
            scrollBody
        }
        .sheet(isPresented: $showScanner) { ScannerSheet { handleScanned($0) } }
        .sheet(item: Binding(get: { inviteCode.map { IdentifiedString($0) } }, set: { inviteCode = $0?.value })) { item in
            QRSheet(code: item.value)
        }
    }

    private var scrollBody: some View {
        ScrollView(showsIndicators: false) {
            VStack(alignment: .leading, spacing: 14) {
                sectionLabel("Подключиться по коду")
                HStack(spacing: 6) {
                    TextField("КОД СЕТИ", text: $code)
                        .foregroundColor(Theme.fg).autocorrectionDisabled().textInputAutocapitalization(.characters).padding(12)
                    Button { if let s = UIPasteboard.general.string { code = s } } label: {
                        Image(systemName: "doc.on.clipboard").font(.system(size: 17, weight: .semibold)).foregroundColor(Theme.dim)
                            .frame(width: 44, height: 44)
                            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color(hex: 0x2A3244))
                                .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Color.white.opacity(0.08), lineWidth: 1)))
                    }
                    // Подкраска вместо сплошного синего: он спорил с акцентом
                    // всего экрана. Стрелка крупнее соседней — здесь она главная.
                    Button { let c = code; code = ""; app.connectByCode(c) } label: {
                        Image(systemName: "arrow.right").font(.system(size: 21, weight: .bold))
                            .foregroundColor(Theme.accent).frame(width: 52, height: 44)
                            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Theme.accent.opacity(0.16))
                                .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Theme.accent.opacity(0.4), lineWidth: 1)))
                    }
                }
                .padding(5).background(Theme.card).cornerRadius(14)

                Button { showScanner = true } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "qrcode.viewfinder").font(.system(size: 16, weight: .bold)).foregroundColor(Theme.accent)
                        Text("Сканировать QR").font(.system(size: 15, weight: .bold)).foregroundColor(.white)
                    }
                    .frame(maxWidth: .infinity).padding(.vertical, 13)
                    .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Theme.tile)
                        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(Color.white.opacity(0.08), lineWidth: 1)))
                }

                // Недавние показываем ТОЛЬКО пока они онлайн (есть в живом
                // каталоге). Оффлайн-хост исчезает из строки; вернётся в сеть —
                // снова появится. Так не тыкаешь в мёртвый код.
                let onlineRecent = app.recent.filter { app.hostById($0)?.online == true }
                if !onlineRecent.isEmpty {
                    sectionLabel("Недавние")
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            // Подсвечен ВЫБРАННЫЙ (к кому подключены/подключаемся),
                            // а не просто первый.
                            ForEach(onlineRecent, id: \.self) { id in
                                recentChip(id, highlighted: id == app.connectedTo)
                            }
                        }.padding(.horizontal, 1)
                    }
                }

                let shown = app.displayedHosts()
                // Связь с сервером потеряна, а список НЕ пуст: цифры и состав хостов
                // ниже — последние известные, то есть могут врать. Молчать нельзя,
                // иначе устаревший список выглядит как живой. Руками делать ничего
                // не нужно — клиент переподключается сам, об этом и говорим.
                let stale = app.serverOnline == false && !shown.isEmpty
                // Знак у заголовка вместо полосы во всю ширину: та кричала
                // сильнее, чем стоило сообщение, и вдобавок дышала, перетягивая
                // взгляд с самого списка. Где искать поломку, показывает точка
                // на ячейке «Сервер» в нав-баре — видная с любой вкладки.
                // Заголовок здесь НЕ через sectionLabel: тот растянут на всю
                // ширину и, попав в строку, съедал всё место — знак о потере
                // связи выталкивало за край.
                HStack(spacing: 8) {
                    Text("Хосты").foregroundColor(Theme.dim).font(.system(size: 13, weight: .bold))
                    Spacer()
                    if stale { StateChip(text: "последние известные") }
                }.padding(.top, 6)
                if shown.isEmpty {
                    Text(app.serverOnline == false ? "Нет связи с сервером.\nПроверьте адрес во вкладке «Сервер»." : "Хостов пока нет.\nВведите код сети или поднимите свой во вкладке «Хост».")
                        .foregroundColor(Theme.dim).font(.system(size: 14)).multilineTextAlignment(.center).frame(maxWidth: .infinity).padding(.vertical, 40)
                } else {
                    // Данные не живые — показываем это САМИМ СПИСКОМ, а не ещё
                    // одним элементом: приглушённое читается как «неактуально»
                    // мгновенно и без слов.
                    ForEach(shown) { HostCard(host: $0) }
                        .opacity(stale ? 0.55 : 1)
                        .animation(.easeInOut(duration: 0.4), value: stale)
                }
            }
            .padding(20).padding(.top, 2).navPadding()
        }
    }
}

// ── Единый блок статуса: выключен → подключаюсь → подключено ──────────────────
// Один и тот же блок перетекает между состояниями (кольцо, значок, цвет),
// чтобы взгляд не искал статус в новом месте.

struct VPNHero: View {
    @EnvironmentObject var app: AppState
    @Binding var inviteCode: String?
    @State private var pulsing = false
    @State private var copiedInvite = false

    private var tint: Color {
        switch app.vpnState { case 1: return Theme.amber; case 2: return Theme.green; default: return Theme.accent }
    }
    private var icon: String {
        switch app.vpnState { case 1: return "shield.lefthalf.filled"; case 2: return "checkmark.shield.fill"; default: return "shield.slash.fill" }
    }
    private var host: Host? { app.connectedTo.flatMap { app.hostById($0) } }
    private var title: String {
        switch app.vpnState {
        // Уже были подключены и снова состояние 1 → это авто-реконнект (сменилась
        // сеть), а не первый коннект. Показываем это честно.
        case 1: return app.connectedSince != nil ? "Переподключение…" : "Подключаюсь…"
        case 2: return host?.name ?? app.connectedTo ?? "Подключено"
        default: return "VPN выключен"
        }
    }
    // Text, а не String: в подпись вклеивается SF Symbol протокола.
    private var subtitle: Text {
        switch app.vpnState {
        case 1: return Text(host?.name ?? "Пробиваю канал к хосту")
        case 2:
            guard let h = host else { return Text("Канал поднят") }
            return Text(countryLabel(h)) + Text("  ·  ") + symbolText(protoIcon(h.proto), protoName(h.proto))
        default:
            let n = app.displayedHosts().count
            return Text(n == 0 ? "Введите код сети или поднимите свой хост"
                               : "Доступно хостов: \(n) · выберите ниже или жмите «Старт»")
        }
    }

    var body: some View {
        PinnedPanel(tint: tint) {
            if app.vpnState == 2 {
                // Подключено — круг уступает место пользе: сколько идёт, куда,
                // адрес хоста, гости и чем позвать друзей.
                TimelineView(.periodic(from: .now, by: 1)) { _ in
                    StatusLine(icon: "checkmark.shield.fill", title: title,
                               clock: uptimeText(app.connectedSince), tint: tint)
                }
                subtitle.foregroundColor(Theme.dim).font(.system(size: 13))
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let h = host {
                    HStack(spacing: 8) {
                        CopyTile(label: "IP ХОСТА", value: h.ip.isEmpty ? "—" : h.ip)
                        StatTile(label: "ГОСТЕЙ", value: "\(h.guests) / \(h.max)", symbol: "person.2.fill")
                    }
                }
                if let id = app.connectedTo {
                    ShareButtons(code: id, qrCode: $inviteCode).padding(.top, 2)
                    Text("Позвать друзей в эту же сеть").foregroundColor(Theme.dim).font(.system(size: 11))
                }
                // Честная сноска — только там, где туннеля нет (симулятор).
                if !TunnelManager.available {
                    Text("Канал к хосту поднят. Полный туннель — на устройстве с VPN-профилем.")
                        .foregroundColor(Theme.dim.opacity(0.7)).font(.system(size: 11))
                        .multilineTextAlignment(.center)
                }
            } else {
                // Показывать нечего, кроме статуса — круг честно занимает место.
                HeroCircle(icon: icon, tint: tint, pulsing: app.vpnState == 1)
                Text(title).foregroundColor(Theme.fg).font(.system(size: 21, weight: .heavy))
                    .lineLimit(1).minimumScaleFactor(0.6)
                subtitle.foregroundColor(Theme.dim).font(.system(size: 13))
                    .multilineTextAlignment(.center)
                // Разовое сообщение об отказе: отдельно от vpnState, иначе фоновый
                // опрос статуса ядра его затирает (или оставляет навсегда).
                if let err = app.vpnError {
                    Text(err).foregroundColor(Theme.red).font(.system(size: 13))
                        .multilineTextAlignment(.center)
                }
            }
        }
        .animation(.easeInOut(duration: 0.3), value: app.vpnState)
    }
}

struct IdentifiedString: Identifiable { let value: String; var id: String { value }; init(_ v: String) { value = v } }

struct HostCard: View {
    @EnvironmentObject var app: AppState
    let host: Host
    @State private var password = ""
    private var expanded: Bool { app.expandedId == host.id }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { app.expandedId = expanded ? nil : host.id }
                // Раскрыли — меряем ПОКА открыто; закрыли — прекращаем.
                app.watchPing(app.expandedId == host.id ? host : nil)
            } label: {
                HStack(spacing: 12) {
                    flagAvatar
                    VStack(alignment: .leading, spacing: 5) {
                        Text(host.name.isEmpty ? host.id : host.name)
                            .foregroundColor(Theme.fg).font(.system(size: 16, weight: .semibold))
                            .lineLimit(1).minimumScaleFactor(0.8)
                        hostSubtitle(host).foregroundColor(Theme.dim).font(.system(size: 13)).lineLimit(1)
                        capacityBar
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if host.hasPassword { Image(systemName: "lock.fill").foregroundColor(Theme.dim).font(.system(size: 13)) }
                    Image(systemName: expanded ? "chevron.up" : "chevron.right")
                        .foregroundColor(Theme.dim).font(.system(size: 13, weight: .semibold))
                }
            }
            if expanded {
                // Содержимое ПОЯВЛЯЕТСЯ, а не возникает: на Android это делают
                // expandVertically + fadeIn, здесь — тот же смысл. Вход чуть
                // задержан, чтобы карточка успела начать расти и содержимое не
                // «обгоняло» её край; выход быстрый — закрытие должно быть резче
                // открытия, иначе кажется, что карточка залипает.
                // Группируем в один вью: переход вешается на НЕГО, а на `if`
                // его повесить нельзя — это не вью, а ветка сборщика.
                VStack(alignment: .leading, spacing: 12) {
                VStack(spacing: 8) {
                    // Код и IP — тап копирует.
                    HStack(spacing: 8) {
                        CopyTile(label: "КОД", value: host.id)
                        CopyTile(label: "IP", value: host.ip.isEmpty ? "—" : host.ip)
                    }
                    HStack(spacing: 8) {
                        StatTile(label: "СТРАНА", value: countryLabel(host))
                        StatTile(label: "ГОСТЕЙ", value: "\(host.guests) / \(host.max)")
                        PingTile(value: app.pings[host.id] ?? "…")
                    }
                    HStack(spacing: 8) {
                        StatTile(label: "ДОСТУП", value: host.hasPassword ? "по паролю" : "открытый",
                                 symbol: host.hasPassword ? "lock.fill" : "lock.open.fill")
                        StatTile(label: "ПРОТОКОЛ", value: protoName(host.proto), symbol: protoIcon(host.proto))
                    }
                }

                if host.hasPassword {
                    SecureField("Пароль", text: $password).foregroundColor(Theme.fg)
                        .padding(12).background(Theme.tile).cornerRadius(11)
                }
                Button { app.connect(host, password: password) } label: { calmButton("Подключить") }
                    .disabled(!host.usable).opacity(host.usable ? 1 : 0.5)
                }
                .transition(.asymmetric(
                    insertion: .opacity.animation(.easeOut(duration: 0.22).delay(0.06)),
                    removal: .opacity.animation(.easeIn(duration: 0.12))
                ))
            }
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(expanded ? Theme.cardHi : Theme.card)
                .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(expanded ? Theme.accent.opacity(0.28) : Color.white.opacity(0.05), lineWidth: 1))
        )
        // Фон и рамка ПЕРЕТЕКАЮТ, а не переключаются рывком (на Android это
        // animateColorAsState). Без этого подсветка раскрытой карточки
        // «щёлкает», и вся плавность роста высоты пропадает даром.
        .animation(.easeInOut(duration: 0.2), value: expanded)
        // Обрезка по скруглению: пока карточка растёт, содержимое не должно
        // вылезать за её край.
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
    }

    /// Флаг страны крупно слева — как аватарка, на всю высоту строки.
    private var flagAvatar: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Theme.tile)
            RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(Color.white.opacity(0.08), lineWidth: 1)
            Text(hostFlag(host)).font(.system(size: 29))
        }
        .frame(width: 56, height: 56)
    }

    /// Насколько хост заполнен — тонкая полоса под подписью.
    @ViewBuilder private var capacityBar: some View {
        if host.max > 0 {
            GeometryReader { geo in
                let frac = min(1, Double(host.guests) / Double(host.max))
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.white.opacity(0.09)).frame(height: 3)
                    if frac > 0 {
                        Capsule().fill(frac < 0.8 ? Theme.green : Theme.amber)
                            .frame(width: max(3, geo.size.width * frac), height: 3)
                    }
                }
            }
            .frame(height: 3).padding(.top, 1)
        }
    }
}

/// Показать код сети как QR — гость наводит камеру и подключается.
struct QRSheet: View {
    @Environment(\.dismiss) var dismiss
    let code: String
    var body: some View {
        NavigationStack {
            VStack(spacing: 22) {
                Spacer()
                if let img = qrImage(code) {
                    Image(uiImage: img).interpolation(.none).resizable().scaledToFit()
                        .frame(width: 240, height: 240).padding(16).background(.white).cornerRadius(18)
                }
                Text(code).font(.system(size: 26, weight: .heavy, design: .monospaced)).kerning(2)
                    .foregroundColor(Theme.accent).textSelection(.enabled)
                Text("Отсканируйте, чтобы подключиться к этой сети").foregroundColor(Theme.dim).font(.system(size: 13))
                BigCopyButton(value: code).padding(.horizontal, 40)
                Spacer(); Spacer()
            }
            .padding().frame(maxWidth: .infinity).background(Theme.bg)
            .navigationTitle("Приглашение").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Закрыть") { dismiss() } } }
        }
    }
}

// ── ВКЛАДКА «ХОСТ» ───────────────────────────────────────────────────────────

struct HostTab: View {
    @EnvironmentObject var app: AppState
    @State private var qrCode: String? = nil
    private let protos: [(String, String, String)] = [
        ("noise", protoIcon("noise"), protoName("noise")),
        ("noise-obfs", protoIcon("noise-obfs"), protoName("noise-obfs")),
        ("plain", protoIcon("plain"), protoName("plain")),
    ]

    private var tint: Color {
        if app.starting { return Theme.amber }
        if app.hosting { return Theme.green }
        return app.hostError != nil ? Theme.red : Theme.accent
    }
    private var statusTitle: String {
        if app.starting { return "Запускаюсь…" }
        if app.hosting { return "Раздаю" }
        return app.hostError != nil ? "Не удалось начать раздачу" : "Раздача выключена"
    }

    var body: some View {
        VStack(spacing: 0) {
            hero
            scrollBody
        }
        .onAppear { app.ensureHostCode() }
        .sheet(item: Binding(get: { qrCode.map { IdentifiedString($0) } }, set: { qrCode = $0?.value })) { item in
            QRSheet(code: item.value)
        }
    }

    /// Прижатая панель хост-режима — тот же язык, что у VPN и сервера.
    /// Код сети НЕ показывается, пока раздача выключена: давать его некому.
    private var hero: some View {
        PinnedPanel(tint: tint) {
            if app.hosting {
                TimelineView(.periodic(from: .now, by: 1)) { _ in
                    StatusLine(icon: "wifi.router.fill", title: "Раздаю",
                               clock: uptimeText(app.hostStartedAt), tint: tint)
                }
                Text(app.hostCode.isEmpty ? "…" : app.hostCode).foregroundColor(Theme.accent)
                    .font(.system(size: 26, weight: .heavy, design: .monospaced)).kerning(2)
                    .frame(maxWidth: .infinity).textSelection(.enabled)
                // «Новый код» — действие редкое, ему хватает строчки.
                QuietButton(icon: "arrow.triangle.2.circlepath", title: "Новый код") { app.newHostCode() }
                HStack(spacing: 8) {
                    StatTile(label: "ГОСТЕЙ",
                             value: "\(app.myHostInfo?.guests ?? 0) / \(app.myHostInfo?.max ?? app.hostMax)",
                             symbol: "person.2.fill")
                    StatTile(label: "ВИДИМОСТЬ",
                             value: (app.hostPublic && app.hostPassword.isEmpty) ? "публичный" : "по коду",
                             symbol: (app.hostPublic && app.hostPassword.isEmpty) ? "globe" : "eye.slash.fill")
                }
                HStack(spacing: 8) {
                    StatTile(label: "ПРОТОКОЛ", value: protoName(app.hostProtocol), symbol: protoIcon(app.hostProtocol))
                    CopyTile(label: "ВАШ IP", value: app.myHostInfo?.ip.isEmpty == false ? app.myHostInfo!.ip : "—")
                }
                // Поделиться — в НИЗУ панели, прямо под самим кодом.
                if !app.hostCode.isEmpty {
                    ShareButtons(code: app.hostCode, qrCode: $qrCode).padding(.top, 2)
                }
            } else {
                HeroCircle(icon: "wifi.router.fill", tint: tint, pulsing: app.starting)
                Text(statusTitle).foregroundColor(Theme.fg).font(.system(size: 21, weight: .heavy))
                    .multilineTextAlignment(.center).lineLimit(1).minimumScaleFactor(0.7)
                Text(app.starting ? "Пробиваю канал наружу…"
                     : (app.hostError ?? "Станьте выходной точкой для друзей"))
                    .foregroundColor(app.hostError != nil && !app.starting ? Theme.red : Theme.dim)
                    .font(.system(size: 13)).multilineTextAlignment(.center)
            }
        }
        .animation(.easeInOut(duration: 0.3), value: app.hosting)
    }

    private var scrollBody: some View {
        ScrollView(showsIndicators: false) {
            VStack(alignment: .leading, spacing: 14) {
                Text("Раздайте свой интернет: \(Platform.device) станет выходной точкой для гостей.")
                    .foregroundColor(Theme.dim).font(.system(size: 13))

                sectionLabel("Имя хоста (видно в каталоге)")
                TextField("Имя", text: $app.hostName)
                    .foregroundColor(Theme.fg).padding(14).background(Theme.card).cornerRadius(14)
                    .onChange(of: app.hostName) { _ in app.applyHostDebounced() }

                // ПАРОЛЬ ⇒ СЕТЬ ВСЕГДА СКРЫТАЯ. Правило соблюдает ядро (см.
                // build_announce), но интерфейс об этом молчал: человек ставил
                // пароль, переключатель продолжал гореть «Публичный», и он был
                // уверен, что сеть в списке, — а её там нет. Показываем ФАКТ,
                // а не сохранённое желание.
                let locked = !app.hostPassword.isEmpty
                let publicNow = app.hostPublic && !locked
                sectionLabel("Видимость")
                HStack(spacing: 8) {
                    bigChip("globe", "Публичный", on: publicNow) {
                        guard !locked else { return }   // с паролем выбора нет
                        app.hostPublic = true; app.applyHostNow()
                    }
                    bigChip("eye.slash.fill", "Скрытый", on: !publicNow) {
                        guard !locked else { return }
                        app.hostPublic = false; app.applyHostNow()
                    }
                }
                .opacity(locked ? 0.55 : 1)            // видно, что выбор сейчас недоступен
                hint(locked
                     ? "С паролем сеть всегда скрыта: публичная карточка светит её существование, страну и адрес — как раз тем, от кого вы закрылись."
                     : publicNow
                     ? "Виден всем в списке хостов — подключиться сможет любой."
                     : "В списке не виден — подключиться можно только по коду выше.")

                HStack {
                    Text("Лимит гостей").foregroundColor(Theme.dim).font(.system(size: 13, weight: .bold))
                    Spacer()
                    Text("\(app.hostMax)").foregroundColor(Theme.accent)
                        .font(.system(size: 17, weight: .heavy, design: .monospaced))
                }.padding(.top, 6)
                // Применяем ТОЛЬКО по отпусканию ползунка — иначе поток сокет-запросов.
                Slider(value: Binding(get: { Double(app.hostMax) }, set: { app.hostMax = Int($0) }), in: 1...256, step: 1,
                       onEditingChanged: { editing in if !editing { app.applyHostNow() } })
                    .tint(Theme.accent)
                HStack(spacing: 6) {
                    ForEach([4, 8, 16, 32, 64, 128], id: \.self) { v in
                        Button { app.hostMax = v; app.applyHostNow() } label: {
                            Text("\(v)").font(.system(size: 13, weight: .bold)).foregroundColor(app.hostMax == v ? .white : Theme.dim)
                                .frame(maxWidth: .infinity).padding(.vertical, 8)
                                .background(app.hostMax == v ? AnyShapeStyle(Theme.accent) : AnyShapeStyle(Theme.cardSel)).cornerRadius(10)
                        }
                    }
                }

                sectionLabel("Пароль (пусто = без пароля)")
                TextField("без пароля", text: $app.hostPassword)
                    .foregroundColor(Theme.fg).padding(14).background(Theme.card).cornerRadius(14)
                    .onChange(of: app.hostPassword) { _ in app.applyHostDebounced() }

                sectionLabel("Протокол")
                HStack(spacing: 8) {
                    ForEach(protos, id: \.0) { pid, icon, name in
                        bigChip(icon, name, on: app.hostProtocol == pid) { app.hostProtocol = pid; app.applyHostNow() }
                    }
                }
                hint(protoDesc(app.hostProtocol), warn: app.hostProtocol == "plain")

                // Кнопки «Стать хостом» тут больше нет: она переехала в нав-бар,
                // по общему правилу «ячейка ведёт на вкладку, а на своей вкладке
                // становится включателем». Раньше главное действие лежало под
                // всеми настройками — до него надо было домотать.
                Text("На \(Platform.deviceName) фоновая раздача ограничена системой — держите приложение открытым, пока раздаёте.")
                    .foregroundColor(Theme.dim).font(.system(size: 12))
            }
            .padding(20).padding(.top, 2).navPadding()
        }
    }

    /// Толстый чип: значок сверху, название снизу — палец попадает не глядя.
    private func bigChip(_ icon: String, _ name: String, on: Bool, _ tap: @escaping () -> Void) -> some View {
        VStack(spacing: 5) {
            Image(systemName: icon).font(.system(size: 18, weight: .semibold)).foregroundColor(on ? .white : Theme.dim)
            Text(name).font(.system(size: 13, weight: .bold)).foregroundColor(on ? .white : Theme.dim)
                .lineLimit(1).minimumScaleFactor(0.8)
        }
        .frame(maxWidth: .infinity).padding(.vertical, 14)
        .background(on ? AnyShapeStyle(LinearGradient(colors: [Theme.accent, Theme.accent2], startPoint: .top, endPoint: .bottom))
                       : AnyShapeStyle(Theme.card))
        .cornerRadius(14)
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(on ? Color.clear : Color.white.opacity(0.07), lineWidth: 1))
        .shadow(color: on ? Theme.accent.opacity(0.3) : .clear, radius: 8, y: 3)
        .contentShape(Rectangle())
        .onTapGesture { UIImpactFeedbackGenerator(style: .light).impactOccurred(); tap() }
    }

    /// Пояснение к выбранному варианту — мелко и не навязчиво, янтарём если риск.
    private func hint(_ t: String, warn: Bool = false) -> some View {
        Text(t).foregroundColor(warn ? Theme.amber : Theme.dim).font(.system(size: 12))
            .frame(maxWidth: .infinity, alignment: .leading)
            .animation(.easeInOut(duration: 0.2), value: t)
    }
}
