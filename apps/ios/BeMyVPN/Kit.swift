import SwiftUI
import UIKit
import CoreImage.CIFilterBuiltins

// ── НАБОР ДЕТАЛЕЙ ────────────────────────────────────────────────────────────
//
// Здесь то, из чего экраны СОБРАНЫ: плитки, кнопки, диск состояния, парящая
// панель и мелкие переводчики «число справочника → картинка». Сами экраны
// (вкладки, карточка хоста, лист приглашения) остались в ContentView.swift.
//
// Шов ровно один — ДЕТАЛЬ ПРОТИВ ЭКРАНА. Любой другой (по вкладкам, по
// «мелкому и крупному») пришлось бы разбирать заново на каждой новой детали.
// Отделка парящего блока и его ореол вынесены дальше, в Halo.swift: они
// держатся друг за друга, а до остального набора им дела нет.

/// Значок защиты по УРОВНЮ (варианты view::Protection) — семейство ЩИТА.
///
/// Уровень считает справочник, картинку выбирает оболочка: наборы значков у
/// четырёх оболочек разные, общего имени у них нет. Сюда приезжает число, а
/// какому протоколу оно соответствует — не наше дело; свой разбор имён здесь
/// был второй копией правила и расходился с ней.
///
/// Семейства значков не пересекаются, иначе один значок читается двумя
/// способами:
///   щит  — защита (протокол): есть / замаскирована / нет
///   глаз — видимость хоста в списке
///   замок — доступ (пароль)
/// `eye.slash` тут был ошибкой: он читается как «скрыт из списка», хотя
/// уровень 1 — про сам трафик, а не про видимость хоста.
func protectionIcon(_ level: Int) -> String {
    switch level {
    case 0: return "lock.shield.fill"    // защищено
    case 1: return "theatermasks.fill"   // защищено и переодето
    case 2: return "shield.slash.fill"   // защиты нет
    default: return "questionmark.circle"
    }
}

/// Цвет по уровню тревоги (варианты view::Alarm) — пороги задаёт справочник.
///
/// ОДНА ЛИНЕЙКА НА ВСЕ ПИНГИ — и до хоста, и до координатора. Раньше их было
/// две, обе списаны с правила руками, и меньшее число выходило тревожнее
/// большего: 137 мс до хоста краснело, 162 мс до координатора числилось
/// «хорошо».
///
/// АКЦЕНТА ТУТ НЕТ ВОВСЕ: мята в этом приложении значит «работает», а не
/// «быстро». Норма молчит — обычный цвет текста; «нет ответа» приглушено —
/// тревожить нечем.
func alarmColor(_ alarm: Int) -> Color {
    switch alarm {
    case 1: return Theme.amber
    case 2: return Theme.red
    case 3: return Theme.dim
    default: return Theme.fg
    }
}

/// ШИРИНА КОРОБКИ ЗНАЧКА ПРОТОКОЛА В СТРОКЕ ИМЕНИ ХОСТА.
///
/// У SF Symbols ширина СВОЯ у каждого знака, и на кегле 12 замер по снимку дал:
/// `theatermasks.fill` — 20.7pt рисунка, `shield.slash.fill` — 12.0pt. Имена
/// хостов в списке из-за этого начинались с разных отступов (132.0pt против
/// 121.7pt), и столбец рвался.
///
/// 22, а не 16: коробка должна ВМЕЩАТЬ самый широкий знак. Уже — и маска
/// полезла бы из своей ячейки на имя. Высота остаётся по рисунку: подрезать
/// знак по квадрату значило бы менять сам рисунок.
///
/// В ПЛИТКАХ ЭТОЙ КОРОБКИ НЕТ, И ЭТО ЗАМЕР. Содержимое плитки центрируется, и
/// каждая плитка центрируется сама по себе — в одну вертикаль соседние
/// значения всё равно не встают, слова разной длины. Зато фиксированная ширина
/// там ВИДНА: узкий замок в «ДОСТУП» отходил от своего слова на 10.7pt против
/// 7.3pt у широкой маски в «ПРОТОКОЛ». Выравнивать нечего, а воздух лишний.
let protoBox: CGFloat = 22

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

struct IdentifiedString: Identifiable { let value: String; var id: String { value }; init(_ v: String) { value = v } }

/// ДИСК СОСТОЯНИЯ — ОДНА ДЕТАЛЬ НА ВСЕ РАЗМЕРЫ.
///
/// Заливка `discFill` плюс кольцо `discRing` были написаны девятью копиями с
/// диаметрами 38/72/74/84/88 — по одной на каждое место, где диск понадобился.
///
/// Размер здесь ЕДИНСТВЕННЫЙ параметр — `d`. Ни своей заливки, ни толщины
/// кольца, ни тени: как только у диска появится второй способ выглядеть,
/// начнётся вторая копия. Значок кладёт вызывающий.
struct StateDisc<Icon: View>: View {
    let tint: Color
    var d: CGFloat = 38
    /// Расходящаяся волна — «идёт процесс». Всегда 1.2с и ×1.28.
    var pulsing = false
    @ViewBuilder let icon: Icon
    var body: some View {
        ZStack {
            if pulsing {
                // Фаза волны считается от ЧАСОВ, а не от анимации состояния.
                // Так пульс сделан и на десктопе (animation-tick), и на Android
                // (rememberInfiniteTransition) — одна механика на три оболочки.
                // Своего состояния не держит и не перезапускается на смене
                // `pulsing`.
                TimelineView(.animation) { ctx in
                    let t = ctx.date.timeIntervalSinceReferenceDate
                        .truncatingRemainder(dividingBy: 1.2) / 1.2
                    Circle().stroke(tint.opacity(0.5), lineWidth: 2)
                        .scaleEffect(1 + 0.28 * t).opacity(0.7 * (1 - t))
                }
            }
            Circle().fill(Theme.discFill(tint))
            Circle().stroke(Theme.discRing(tint), lineWidth: 1)
            icon
        }
        .frame(width: d, height: d)
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
                    // Ширина ЕСТЕСТВЕННАЯ (см. `protoBox`): выравнивать в плитке
                    // нечего, а фиксированная коробка только разводит узкий знак
                    // с его словом.
                    Image(systemName: symbol).font(.system(size: 12, weight: .semibold)).foregroundColor(valueColor)
                }
                if !value.isEmpty {
                    Text(value).foregroundColor(valueColor)
                        .font(.system(size: 13.5, weight: .semibold, design: mono ? .monospaced : .default))
                        // ЦИФРЫ ТАБУЛЯРНЫЕ (одинаковой ширины). Пинг обновляется
                        // раз в секунду, а у пропорциональных цифр «24 мс» и
                        // «38 мс» ширина разная — строка здесь центрируется, и
                        // значение дёргалось влево-вправо на каждом замере.
                        .monospacedDigit()
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
///
/// `fill` — нейтральная ступень s3, ОДНА И ТА ЖЕ у плиток панели состояния и у
/// плиток внутри раскрытой карточки хоста. Ручка оставлена затем, что раскрытая
/// карточка задаёт заливку одним местом на всю сетку (см. `tileFill` в
/// `HostCard`), а не семью вызовами по отдельности.
private func tileBackground(_ accent: Color?, fill: Color = Theme.tile) -> some View {
    RoundedRectangle(cornerRadius: 12, style: .continuous)
        .fill(fill)
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous)
            .stroke(accent ?? Theme.hairline, lineWidth: 1))
}

/// Строка состояния для РАБОТАЮЩЕГО режима: значок кружком, название, часы.
///
/// Диск здесь вчетверо меньше геройского — тот же `StateDisc`, только с другим
/// `d`. Значок никуда не девается и в работе: он опознаёт экран с одного
/// взгляда. Но держать 72pt картинки там, где нужны код и цифры, расточительно:
/// панель прижата к верху и висит на экране постоянно.
struct StatusLine: View {
    let icon: String
    let title: String
    var clock: String? = nil
    let tint: Color
    var body: some View {
        HStack(spacing: 10) {
            StateDisc(tint: tint) {
                Image(systemName: icon).font(.system(size: 17, weight: .semibold)).foregroundColor(tint)
            }
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
            button(copied ? "checkmark" : "doc.on.doc", copied ? "Скопировано" : "Скопировать", done: copied) {
                UIPasteboard.general.string = code
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                withAnimation(.easeOut(duration: 0.15)) { copied = true }
                DispatchQueue.main.asyncAfter(deadline: .now() + Theme.copiedMs) { withAnimation { copied = false } }
            }
            button("qrcode", "QR-код", done: false) { qrCode = code }
        }
    }
    /// `done` — «скопировано». ЦВЕТОМ его больше не отличить: подпись кнопки и
    /// так акцентная. Отличают галочка, слово и рамка вдвое ярче.
    private func button(_ icon: String, _ title: String, done: Bool, _ tap: @escaping () -> Void) -> some View {
        Button(action: tap) {
            HStack(spacing: 8) {
                Image(systemName: icon).font(.system(size: 15, weight: .bold))
                Text(title).font(.system(size: 14, weight: .bold)).lineLimit(1).minimumScaleFactor(0.7)
            }
            .foregroundColor(Theme.accent)
            .frame(maxWidth: .infinity).frame(height: 48)
            // Ступень s3 — та же, что у плиток рядом: кнопка живёт только внутри
            // парящей панели, а панель светлее карточек списка (см. ShareButton
            // в components.slint).
            .background(RoundedRectangle(cornerRadius: 15, style: .continuous).fill(Theme.tile)
                .overlay(RoundedRectangle(cornerRadius: 15, style: .continuous)
                    .stroke(done ? Theme.edgeDone() : Theme.accent.opacity(0.24), lineWidth: 1)))
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
            DispatchQueue.main.asyncAfter(deadline: .now() + Theme.copiedMs) { withAnimation { did = false } }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: did ? "checkmark" : icon).font(.system(size: 12, weight: .bold))
                Text(did ? "Готово" : title).font(.system(size: 12.5, weight: .bold))
            }
            .foregroundColor(did ? Theme.accent : Theme.dim)
            .frame(maxWidth: .infinity).frame(height: 34)
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(did ? Theme.edgeDone() : Theme.hairline, lineWidth: 1))
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

/// ПАРЯЩАЯ ПАНЕЛЬ состояния: наложение поверх прокрутки (`.safeAreaInset` на
/// месте вызова), а не сосед сверху. Список идёт во всю высоту ПОД ней и виден
/// в зазорах вокруг карточки; статус при этом не уезжает.
///
/// Соседом в `VStack` панель занимала собственную полосу во всю ширину: список
/// упирался в её низ, а по бокам от скруглённой карточки стояла пустая полоса
/// цвета страницы — тот самый «квадрат с фоном как у основы», на который
/// жаловались.
struct PinnedPanel<Content: View>: View {
    let tint: Color
    @ViewBuilder let content: Content
    /// Высота содержимого по замеру. `nil` — ещё не мерили: панель встаёт своей
    /// высотой, без «разворачивания» на запуске.
    @State private var h: CGFloat? = nil
    var body: some View {
        VStack(spacing: 8) { content }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16).padding(.horizontal, 18)
            // СОДЕРЖИМОЕ ВСЕГДА СЧИТАЕТ СВОЮ ВЫСОТУ, а не ту, что предлагает
            // зажатая рамка ниже по цепочке, — иначе замер зациклится сам на
            // себя: рамка сжимает, замер видит сжатое, рамка сжимает ещё.
            .fixedSize(horizontal: false, vertical: true)
            .background(GeometryReader { g in
                // ПЕРВЫЙ замер — без плавности и ТОЛЬКО первый: если сюда
                // прилетит готовая высота в обход анимации, панель прыгнет.
                Color.clear.onAppear { if h == nil { h = g.size.height } }
                    // Плавность — транзакцией на самом присвоении. Модификатор
                    // `.animation(_:value:)` на рамке эту смену не ловит: замер
                    // приходит из прохода разметки (проверено на записи — высота
                    // менялась за ОДИН кадр).
                    .onChange(of: g.size.height) { new in
                        if h == nil { h = new }
                        else { withAnimation(.easeInOut(duration: 0.18)) { h = new } }
                    }
            })
            // ЕДЕТ ИМЕННО РАЗМЕТОЧНАЯ ВЫСОТА, а не только нарисованная. Это и
            // держит содержимое под панелью: она подвешена через
            // `safeAreaInset`, и верхний отступ прокрутки считается от этой
            // рамки — замер по записи: зазор 90–92px во всё время хода.
            .frame(height: h, alignment: .top)
            // КРОМКА НЕ РЕЖЕТ, А ГАСИТ. Пока рамка догоняет выросшее содержимое,
            // лишнее нельзя выпускать на фон страницы — но и обрубать его
            // жёстко нельзя: на записи было видно, как нижний край панели режет
            // пополам слова на кнопках («Скопировать», «QR-код»). Ровно на это
            // жаловались на Android. Поэтому вместо `.clipped()` — маска с
            // растворением на последних 16pt рамки.
            //
            // В ПОКОЕ НА ВИД НЕ МЕНЯЕТСЯ НИЧЕГО: там ровно те же 16pt пустого
            // вертикального отступа панели, гасить в них нечего.
            .mask(VStack(spacing: 0) {
                Color.black
                LinearGradient(colors: [.black, .clear], startPoint: .top, endPoint: .bottom)
                    .frame(height: 16)
            })
            // ОТДЕЛКА — ТА ЖЕ, ЧТО У НАВ-БАРА (floatSurface): два парящих слоя
            // должны читаться одним языком. Тень навешена ВНУТРИ `.background`,
            // на саму фигуру: снаружи она легла бы и на текст панели.
            //
            // КОЛЬЦО СОСТОЯНИЯ ТИШЕ СОДЕРЖИМОГО (0.30, было 0.45): на 0.45
            // обводка была ярче самой панели, и вся «высота» держалась на ней.
            // Покажут состояние иначе — панель не станет от этого плоской.
            .background(floatSurface(stroke: tint.opacity(0.30)))
            // СНИЗУ РОВНО ВЫЛЕТ ОРЕОЛА, и это не про воздух. `safeAreaInset`
            // обрезает всё, что вылезает за его границу, а граница — это отступ
            // обёртки: при прежних 12 гашение обрывалось на 12pt под панелью
            // жёсткой кромкой (замер: скачок 31/255 на ровной поверхности).
            // Прибавка снята с верхнего отступа вкладок, поэтому на вид не
            // изменилось ничего.
            .padding(.horizontal, 20).padding(.top, 20).padding(.bottom, veilWidth)
            // ЦВЕТ СОСТОЯНИЯ ПЕРЕТЕКАЕТ, а не переключается: кольцо панели, знак
            // и подписи меняют цвет тем же ходом и за то же время, что и высота.
            // Ключ — сам `tint`: это и есть состояние, покрашенное в цвет
            // (выключено — dim, идёт процесс — amber, работает — accent, отказ —
            // red). Пинг и часы обновляются раз в секунду, `tint` от этого не
            // меняется — панель на них не шевелится.
            //
            // Разметку это НЕ трогает: у обеих веток явный
            // `.transition(.identity)` (см. VPNHero/HostTab/ServerTab) — они
            // меняются мгновенно и на месте. Растворять ветку в ветку внутри
            // стопки нельзя: на время перехода в ней лежали бы ОБЕ разметки,
            // высота подскакивала бы до суммы, а надпись статуса ехала бы вбок
            // из строки в центр — ровно та жалоба, из-за которой анимацию тут
            // когда-то запретили целиком.
            .animation(.easeInOut(duration: 0.18), value: tint)
    }
}

/// Крупный круг статуса — для состояний, где показывать больше нечего.
/// Тот же `StateDisc`, что и в строке состояния, только вдвое крупнее.
struct HeroCircle: View {
    let icon: String
    let tint: Color
    var pulsing = false
    var body: some View {
        StateDisc(tint: tint, d: 72, pulsing: pulsing) {
            Image(systemName: icon).font(.system(size: 30, weight: .semibold)).foregroundColor(tint)
        }
        // 74, а не 72: два пикселя воздуха вокруг диска были и раньше — панель
        // считает по ним свою высоту.
        .frame(height: 74)
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

/// Крупная кнопка «скопировать» под QR-кодом. Спокойная, как и все остальные:
/// подкраска + рамка + цветной текст. Сплошной градиент был единственным ярким
/// пятном на почти пустом экране и перетягивал взгляд с самого кода.
struct BigCopyButton: View {
    let value: String
    @State private var copied = false
    var body: some View {
        Button {
            guard !value.isEmpty else { return }
            UIPasteboard.general.string = value
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            withAnimation(.easeOut(duration: 0.15)) { copied = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + Theme.copiedMs) { withAnimation { copied = false } }
        } label: {
            HStack(spacing: 8) {
                Image(systemName: copied ? "checkmark" : "doc.on.doc").font(.system(size: 15, weight: .bold))
                Text(copied ? "Скопировано" : "Скопировать код").fontWeight(.bold)
            }
            .foregroundColor(Theme.accent)
            .frame(maxWidth: .infinity).padding(.vertical, 15)
            .background(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(Theme.picked())
                    .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .stroke(Theme.edge(bright: copied), lineWidth: 1))
            )
        }.buttonStyle(PressStyle())
    }
}

struct PingTile: View {
    /// Подпись и уровень тревоги ПАРОЙ, как их отдаёт справочник (см. `Ping`).
    /// Порознь они и разъезжались: подпись брали отсюда, а цвет считали, разобрав
    /// эту же подпись обратно в число.
    let ping: Ping
    /// См. `tileBackground`: нейтральная s3 — и в панели, и в раскрытой карточке.
    var fill: Color = Theme.tile
    private var waiting: Bool { ping == .measuring }
    /// Приглушённая тревога и есть «не ответил» (view::Alarm). Первый замер
    /// отсеян выше: у него та же тревога, но своя ветка.
    private var noAnswer: Bool { ping.alarm == 3 }
    @State private var spin = false

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
                .background(tileBackground(nil, fill: fill))
            } else if noAnswer {
                // Только знак, без слова: перечёркнутая антенна говорит сама,
                // а «нет» рядом с ней — это то же самое ещё раз.
                StatTile(label: "ПИНГ", value: "", symbol: "antenna.radiowaves.left.and.right.slash",
                         tint: alarmColor(ping.alarm), fill: fill)
            } else {
                StatTile(label: "ПИНГ", value: ping.text, tint: alarmColor(ping.alarm), fill: fill)
            }
        }
        // АНИМАЦИИ ПО ЗАМЕРУ ЗДЕСЬ НЕТ, И ЭТО НАМЕРЕННО. Стояла
        // `.animation(.easeOut(0.25), value: value)`, и она делала ровно то, на
        // что жаловались: ветки `waiting`/`no-answer`/число — это РАЗНЫЕ вьюхи,
        // при смене SwiftUI играет переход по умолчанию (растворение), а
        // одновременная перецентровка строки тащит текст вбок. Замер приходит
        // раз в секунду — то есть «уезжает и растворяется» повторялось
        // ежесекундно. Смена значения обязана быть мгновенной и на месте;
        // движение в этой плитке осталось только у стрелки «идёт замер».
    }
}

struct StatTile: View {
    let label: String; let value: String
    var symbol: String? = nil
    var tint: Color = Theme.fg
    /// См. `tileBackground`: нейтральная s3 — и в панели, и в раскрытой карточке.
    var fill: Color = Theme.tile
    var body: some View {
        TileBody(label: label, value: value, symbol: symbol, valueColor: tint) { EmptyView() }
            .background(tileBackground(nil, fill: fill))
    }
}

/// Плитка со значением, которое копируется тапом (код, IP).
/// От обычной отличается только акцентной иконкой — не рамкой и не фоном,
/// иначе плитки в одном блоке выглядят разнородными.
struct CopyTile: View {
    let label: String; let value: String
    /// См. `tileBackground`: нейтральная s3 — и в панели, и в раскрытой карточке.
    var fill: Color = Theme.tile
    @State private var copied = false
    private var empty: Bool { value.isEmpty || value == "—" }
    var body: some View {
        Button {
            guard !empty else { return }
            UIPasteboard.general.string = value
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            withAnimation(.easeOut(duration: 0.15)) { copied = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + Theme.copiedMs) { withAnimation { copied = false } }
        } label: {
            TileBody(label: label, value: copied ? "Скопировано" : value,
                     valueColor: copied ? Theme.accent : Theme.fg, mono: !copied) {
                if !empty {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .foregroundColor(Theme.accent).font(.system(size: 11, weight: .bold))
                }
            }
            .background(tileBackground(copied ? Theme.edgeDone() : nil, fill: fill))
        }.buttonStyle(.plain)
    }
}

func sectionLabel(_ t: String) -> some View {
    Text(t).foregroundColor(Theme.dim).font(.system(size: 13, weight: .bold))
        .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 6)
}

// Ниже плавает нав-бар; он приподнят над краем на 42pt (было 34), поэтому
// отступ подрос на ту же величину — иначе последняя карточка пряталась бы.
// Правило: низ ПОКОЯЩЕГОСЯ содержимого = верх завесы нав-бара, иначе последняя
// карточка гасла бы, стоя на месте. 108 = 104 + 4 при завесе `veilWidth` 18
// (было 114 = 104 + 10 при завесе 24).
///
/// ЧИСЛО НАЗВАНО, А НЕ ВПИСАНО В МОДИФИКАТОР: его читает не только отступ
/// списка, но и умная прокрутка — она подтягивает раскрытую карточку ВМЕСТЕ с
/// этим клиренсом, иначе карточка «видима», упираясь низом в край экрана, то
/// есть лёжа под баром.
let navClearance: CGFloat = 108
extension View { func navPadding() -> some View { self.padding(.bottom, navClearance) } }
/// Главная кнопка — ОДНА НА ВСЁ ПРИЛОЖЕНИЕ, спокойная: подкраска + рамка +
/// цветной текст, тот же приём, что у ShareButtons и у действующей ячейки
/// нав-бара.
///
/// Градиентного близнеца здесь больше нет. Сплошная синяя плашка во всю ширину
/// кричала громче всего на экране и перетягивала внимание с того, ради чего на
/// экран смотрят: в карточке хоста — с его же цифр, на вкладке «Сервер» — с
/// адреса и состояния связи. Один стиль на все кнопки — ещё и способ не решать
/// каждый раз заново, какая из них «главнее».
func calmButton(_ title: String) -> some View {
    Text(title).fontWeight(.bold).foregroundColor(Theme.accent)
        .frame(maxWidth: .infinity).padding(.vertical, 15)
        .background(
            // Та же подкраска, что у выбранного чипа: кнопка тоже лежит на
            // странице, и прозрачным акцентом она смешивалась с почти чёрным
            // фоном — выходила ТЕМНЕЕ соседнего поля ввода (13.6 против 15.5 по
            // L*) и читалась вдавленной.
            RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Theme.picked())
                .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .stroke(Theme.edge(), lineWidth: 1))
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
/// Значка протокола здесь НЕТ: он переехал в строку с именем хоста (см.
/// `HostCard`), потому что защита относится к самому хосту, а не к счётчику
/// гостей.
func hostSubtitle(_ h: Host) -> Text {
    var parts: [String] = []
    if let cc = GeoFlags.countryOf(h.ip) { parts.append(cc) }
    // Потолок хост может и не объявить (0) — дробь «1/0» в этом случае врёт.
    parts.append(h.max > 0 ? "гостей \(h.guests)/\(h.max)" : "гостей \(h.guests)")
    return Text(parts.joined(separator: " · "))
}
