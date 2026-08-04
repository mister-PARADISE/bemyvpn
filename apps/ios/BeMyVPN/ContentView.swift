import SwiftUI
import UIKit
import CoreImage.CIFilterBuiltins

/// Имя протокола по-человечески — без крипто-жаргона, одним словом.
func protoName(_ p: String) -> String {
    switch p {
    // ПУСТАЯ СТРОКА — ЭТО «ОБЫЧНЫЙ», А НЕ «БЕЗ ШИФРА». Хост, не объявивший
    // протокол в каталоге, всё равно поднимает шифрованный канал (значение
    // по умолчанию у ядра). Раньше телефон подписывал такой хост как
    // незашифрованный — то есть ВРАЛ про защищённый хост, что он голый, — и
    // та же запись каталога в окне и в терминале называлась «Обычный».
    case "", "noise", "noise-aes": return "Обычный"
    case "noise-obfs": return "Маскировка"
    case "plain": return "Без шифра"
    // Незнакомое имя показываем как есть: врать «Без шифра» про неизвестный
    // протокол так же неверно, как врать про пустой.
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
    // Пустая строка — «Обычный», см. `protoName`: хост без объявленного
    // протокола шифрует. Перечёркнутый щит здесь означал бы то же враньё, что
    // и подпись «Без шифра», только картинкой — а её замечают раньше слов.
    case "", "noise", "noise-aes": return "lock.shield.fill"   // защищено
    case "noise-obfs": return "theatermasks.fill"              // защищено и переодето
    case "plain": return "shield.slash.fill"                   // защиты нет
    default: return "questionmark.circle"
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
            // ПОЛОСКА НАД ПАНЕЛЬЮ. Панель отстоит от верхнего края на 20pt, и в
            // этот просвет всплывало содержимое прокрутки: «Подключиться по
            // коду» и поле «КОД СЕТИ» выезжали на полной яркости прямо на часы
            // и вырез. Ничего полезного там нет и быть не может — просто фон.
            //
            // Живёт ЗДЕСЬ, а не в отделке панели: панель подвешена через
            // `safeAreaInset`, и он обрезает всё, что вылезает за его границу.
            // В корневом ZStack обрезать некому, а порядок даёт нужную укладку:
            // выше вкладки с её прокруткой, ниже нав-бара.
            //
            // 20pt — ровно верхний отступ обёртки панели, а безопасную зону над
            // ними добирает подложка через `ignoresSafeArea`: числа «на глазок»
            // под разные вырезы здесь не нужны. Вешать `ignoresSafeArea` на сам
            // VStack нельзя — тогда 20pt лягут от края экрана, а не от края
            // безопасной зоны, и просвет останется.
            VStack(spacing: 0) {
                Theme.bg
                    .frame(height: 20)
                    .background(Theme.bg.ignoresSafeArea(.container, edges: .top))
                Spacer(minLength: 0)
            }
            .allowsHitTesting(false)
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
        // ВТОРОЙ ПАРЯЩИЙ СЛОЙ: отделка та же, что у панели состояния
        // (floatSurface). Свой цвет, подобранный на глаз, разошёлся бы с
        // панелью при первой же правке темы. `up` — бар прижат снизу.
        .background(floatSurface(radius: 28, stroke: Theme.hairlineFloat, up: true))
        .padding(.horizontal, 18)
        // safe-area пробита на корне (ContentView), поэтому зазор снизу задаём
        // руками. Нужно ОДНОВРЕМЕННО: (1) не влезать в зону home-indicator внизу
        // и (2) не «висеть» высоко над краем. 42pt (было 34) — бар заметно
        // приподнят над краем, но не отрывается. Прибавка идёт СВЕРХ безопасной
        // зоны, поэтому на устройствах с индикатором и без него ощущение одно.
        .padding(.bottom, 42)
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
        // ТО ЖЕ «ВЫБРАННОЕ», ЧТО И ВЕЗДЕ (Theme.picked), И ТЕПЕРЬ С РАМКОЙ.
        // Ячейка сидит на парящей панели, но красится ступенью s1 с подкраской,
        // как чипы на странице: подкраска в полную силу поверх самой панели
        // сделала бы её светлее всего экрана. Общий цвет выбранного даёт L* 19.5
        // при панели 15.9 — ячейка поднята над своей подложкой на 3.6, видно, что
        // она приподнята, а подпись держит 7.84 при пороге 4.5.
        // Рамка появилась затем, чтобы «я на этой вкладке» отличалось не одним
        // лишь цветом: раньше у ячейки-перехода её не было вовсе.
        .background(active ? RoundedRectangle(cornerRadius: innerR, style: .continuous).fill(Theme.picked())
            .overlay(RoundedRectangle(cornerRadius: innerR, style: .continuous).stroke(Theme.edge(), lineWidth: 1)) : nil)
        .overlay(alignment: .top) {
            if let live {
                Circle().fill(live).frame(width: 7, height: 7)
                    .overlay(Circle().stroke(Theme.float, lineWidth: 2))
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
            RoundedRectangle(cornerRadius: innerR, style: .continuous).fill(Theme.picked(hue))
                .overlay(RoundedRectangle(cornerRadius: innerR, style: .continuous).stroke(Theme.edge(hue), lineWidth: 1))
        )
        .contentShape(Rectangle())
        .onTapGesture(perform: tap)
    }

    // «Сервер» — навигация всегда: у вкладки нет своего включателя. Зато на ней
    // горит состояние связи, видное С ЛЮБОЙ вкладки. Когда всё хорошо — точки
    // нет: молчание и есть «ок». Раньше об обрыве кричала полоса на вкладке VPN.
    private var serverCell: some View {
        // Обрыв — янтарь, как и «проверяю»: он чинится сам за секунды.
        cell(.server, icon: "server.rack", label: "Сервер",
             live: app.serverOnline == true ? nil : Theme.amber)
    }

    @ViewBuilder private var vpnCell: some View {
        if app.tab != .vpn {
            cell(.vpn, icon: "shield.fill", label: "VPN",
                 live: app.vpnState == 0 ? nil : (app.vpnState == 2 ? Theme.accent : Theme.amber))
        } else if app.vpnState == 0 {
            // КНОПКА И СОСТОЯНИЕ ОДНОГО ЦВЕТА — РАЗЛИЧАЕТ ФОРМА. Мята значит и
            // «это можно нажать», и «это работает»; отдельного зелёного больше
            // нет, потому что от мяты его было не отличить. Здесь мята одета
            // КНОПКОЙ: подкраска плюс рамка. Состояние носит другую форму —
            // залитую точку у соседней ячейки, значок в панели. «Стоп» остаётся
            // красным: на кнопке выхода красный понятен без обучения и читается
            // как «прервать», а не как «беда».
            action("bolt.fill", "Старт", hue: Theme.accent) { app.quickConnect() }
        } else {
            action("xmark", app.vpnState == 1 ? "Отмена" : "Стоп", hue: Theme.red) { app.stop() }
        }
    }

    @ViewBuilder private var hostCell: some View {
        if app.tab != .host {
            cell(.host, icon: "wifi.router.fill", label: "Хост",
                 live: (app.hosting || app.starting) ? (app.hosting ? Theme.accent : Theme.amber) : nil)
        } else if app.hosting || app.starting {
            action("xmark", app.starting ? "Отмена" : "Стоп", hue: Theme.red) { app.stopHost() }
        } else {
            // «Раздать» — акцент по тому же правилу, что и «Старт».
            action("power", "Раздать", hue: Theme.accent) { app.becomeHost() }
        }
    }
}

// ── переиспользуемое ──────────────────────────────────────────────────────────

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
                // Запрета анимаций в панели больше нет (см. PinnedPanel), то
                // есть обычный `.repeatForever` здесь бы тоже завёлся; менять
                // не на что: от часов волна идёт ровно так же, а лишнего
                // состояния и перезапуска при смене `pulsing` не требует.
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
            .background(floatSurface(radius: 22, stroke: tint.opacity(0.30)))
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
///
/// Свечения здесь больше нет: параметр `glowing` не передавался НИ РАЗУ, то есть
/// тень всегда рисовалась прозрачной.
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
    let value: String
    /// См. `tileBackground`: нейтральная s3 — и в панели, и в раскрытой карточке.
    var fill: Color = Theme.tile
    private var waiting: Bool { value == "…" }
    private var noAnswer: Bool { value == "—" }
    @State private var spin = false

    /// ОДНА ЛИНЕЙКА НА ОБА ПИНГА (здесь и на вкладке «Сервер»). Раньше их было
    /// две, и меньшее число выходило тревожнее большего: 137 мс до хоста
    /// краснело, 162 мс до координатора числилось «хорошо».
    ///
    /// АКЦЕНТА В ПИНГЕ НЕТ ВОВСЕ: мята в этом приложении значит «работает», а не
    /// «быстро». Норма молчит — обычный цвет текста.
    private var tint: Color {
        guard let ms = Int(value.split(separator: " ").first.map(String.init) ?? "") else { return Theme.fg }
        if ms < 250 { return Theme.fg }
        if ms <= 500 { return Theme.amber }
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
                .background(tileBackground(nil, fill: fill))
            } else if noAnswer {
                // Только знак, без слова: перечёркнутая антенна говорит сама,
                // а «нет» рядом с ней — это то же самое ещё раз.
                StatTile(label: "ПИНГ", value: "", symbol: "antenna.radiowaves.left.and.right.slash",
                         tint: Theme.dim, fill: fill)
            } else {
                StatTile(label: "ПИНГ", value: value, tint: tint, fill: fill)
            }
        }
        // АНИМАЦИИ ПО `value` ЗДЕСЬ НЕТ, И ЭТО НАМЕРЕННО. Стояла
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
/// Вылет гашения от контура парящего блока наружу. Отступы прокрутки считаются
/// ОТ НЕГО — покоящееся содержимое не должно попадать в зону гашения, иначе оно
/// выглядит подтенённым на пустом месте.
/// БЫЛО 24, УКОРОЧЕНО НА ЧЕТВЕРТЬ по просьбе владельца.
let veilWidth: CGFloat = 18

/// Сплошная часть ореола: до неё гашение ПОЛНОЕ, дальше сходит на нет к
/// `veilWidth`.
///
/// ЧИСЛО НЕ ИЗ ВКУСА, А ИЗ ГЕОМЕТРИИ ВЫРЕЗА, И ТЕПЕРЬ ОНО ЕЙ И СЧИТАЕТСЯ. Самая
/// дальняя от контура точка выреза скруглённого угла — сам угол габаритной
/// коробки, и он отстоит от дуги ровно на R·(√2−1): 9.11 при радиусе панели 22 и
/// 11.60 при радиусе бара 28. Гашение, которое начинает спадать прямо от
/// контура, в этой точке даёт уже половину яркости — вырез принципиально не
/// вычистить, пока сплошная часть не перекрывает его ЦЕЛИКОМ.
///
/// Раньше здесь стояло плоское 12 — потолок из двух блоков, взятый с запасом.
/// Теперь считается по своему радиусу: панели хватает 9.61, бару нужно 12.10.
/// Панель от этого гасит на 20% короче в полную силу — ровно та «менее
/// интенсивная» часть, которую просили; бару уступить нечего, его 11.60 и есть
/// физический предел.
///
/// Полпункта сверху — запас на округление до пикселя (у прежнего плоского 12
/// запас у бара был 0.4, тот же порядок).
func haloSolid(_ radius: CGFloat) -> CGFloat { radius * 0.4142136 + 0.5 }
/// Насколько ореол уходит ЗА блок, к ближнему краю экрана. Блок прижат к краю,
/// но не вплотную; в этот просвет содержимое пролезало и обрывалось о край
/// экрана. Ореол закрывает просвет ТЕМ ЖЕ силуэтом — одна форма, без второго
/// механизма и без стыка. С запасом на любой вырез и любую высоту статус-бара:
/// за экраном лишний фон ничего не стоит, а не хватило бы — вышла бы жёсткая
/// кромка поперёк экрана.
let haloBack: CGFloat = 200

/// ОТДЕЛКА ПАРЯЩЕГО БЛОКА: заливка, тихая рамка, короткая тень в сторону
/// подъезда.
///
/// Высоту держит ПЕРЕПАД К ФОНУ: заливка (Theme.float — одна на панель и на
/// нав-бар) светлее страницы на 11.5 по L*. НЕПРОЗРАЧНАЯ: прежние 0.97 не
/// покупали ничего — размытия под блоком нет ни на одной оболочке (родного
/// `.ultraThinMaterial` здесь намеренно нет: с ним одна платформа выглядела бы
/// иначе, чем две другие), зато сквозь панель на снимке в прокрутке читался
/// текст списка под ней. Прежняя дальняя тень (radius 22, y 12) на почти-чёрном
/// фоне размывалась в грязь и не сообщала ничего.
///
/// КРОМКИ СВЕТА 1pt СВЕРХУ БОЛЬШЕ НЕТ. Она была подпоркой под тёмную палитру:
/// на прежних поверхностях блок отличался от фона на 6.4 по L*, и край
/// приходилось обводить руками. Тень тут ни при чём — до чёрного фону 8 единиц
/// из 255, тень углубляет его на 1–3 в ЛЮБОЙ палитре. Перепад теперь 13.5, край
/// виден сам, а полоска сверху просто бросалась в глаза.
///
/// `up` — блок прижат снизу (нав-бар), тень уходит ВВЕРХ, к содержимому. Вниз
/// она уезжала под сам бар, в полосу, на которую никто не смотрит.
func floatSurface(radius: CGFloat, stroke: Color, up: Bool = false) -> some View {
    RoundedRectangle(cornerRadius: radius, style: .continuous)
        .fill(Theme.float)
        .overlay(
            RoundedRectangle(cornerRadius: radius, style: .continuous)
                .stroke(stroke, lineWidth: 1)
        )
        // ТЕНЬ КОРОТКАЯ, НО С ПОЛОГИМ СПАДОМ. Прежняя (radius 5 при 90 %) у самой
        // кромки роняла фон почти в чёрный — край ОБРЫВАЛСЯ, а не спадал.
        // Размытие больше, непрозрачность меньше: вылет тот же, кривая пологая.
        //
        // ПЛОТНОСТЬ УБАВЛЕНА С 0.6 ДО 0.35, РАДИУС И СМЕЩЕНИЕ НЕ ТРОНУТЫ. Замер
        // по снимку симулятора: при 0.6 самая тёмная точка тени была 6/255 при
        // фоне 9/255 — глубина 3 единицы из 255, то есть тень и в полную силу
        // почти ничего не делала. Это не свойство настройки, а свойство фона: до
        // чёрного странице остаётся восемь единиц, глубже падать некуда.
        .shadow(color: .black.opacity(0.35), radius: 8, x: 0, y: up ? -5 : 5)
        // ОРЕОЛ — ПОД всем блоком, включая его чёрную тень: та должна ложиться
        // на уже погашенный фон, ровно как она лежит на пустой странице при
        // коротком списке. `.background` ставится последним именно поэтому.
        .background { floatHalo(radius: radius, up: up) }
}

/// ГАШЕНИЕ — ОРЕОЛ ПО КОНТУРУ БЛОКА, А НЕ ПОЛОСА РЯДОМ С НИМ.
///
/// Полоса гасила только «до» и «после» блока: сбоку от карточки содержимое шло в
/// полную яркость, а в вырезах скруглённых углов пролезало наружу и обрывалось
/// ножом (замер: верхушка «П» из «Подключиться по коду», 5.3×8.3pt). Ореол
/// считает расстояние ДО КОНТУРА — одинаково сверху, снизу, сбоку и в вырезах.
///
/// СОБРАН ИЗ ГРАДИЕНТОВ, А НЕ ИЗ РАЗМЫТИЯ. Размытие даёт гауссиану: у неё нет
/// сплошной части, поэтому вырез угла ею не вычистить (см. `haloSolid`) — а
/// вычистить его и есть задача. Градиенты дают ТОЧНЫЙ ход: сплошное до
/// `haloSolid(radius)` (9.61 у панели, 12.10 у бара), линейно на нет к 18.
///
/// Холст растянут за коробку блока отрицательными отступами: `Canvas` режет по
/// своим границам, поэтому границы и должны включать весь вылет.
///
/// Кликов не перехватывает. На коротком списке невидим по построению: это фон
/// страницы, растворяющийся в фоне страницы, — тенью посреди пустоты он стать не
/// может.
func floatHalo(radius: CGFloat, up: Bool) -> some View {
    Canvas { ctx, size in
        // Нав-бар — тот же ореол, отражённый по вертикали: форма симметрична, и
        // вторая система координат ради этого не нужна.
        if up {
            ctx.translateBy(x: 0, y: size.height)
            ctx.scaleBy(x: 1, y: -1)
        }
        let clear = Theme.bg.opacity(0)   // прозрачный ФОН, а не прозрачный чёрный
        let v = veilWidth, s = haloSolid(radius), r = radius
        // Коробка самого блока внутри холста.
        let x0 = v, y0 = haloBack
        let w = size.width - 2 * v, h = size.height - haloBack - v
        // Высота прямой части: ниже начинаются дуги углов, там считает радиальный.
        let straight = y0 + h - r
        func rect(_ x: CGFloat, _ y: CGFloat, _ rw: CGFloat, _ rh: CGFloat) -> Path {
            Path(CGRect(x: x, y: y, width: rw, height: rh))
        }
        let out = Gradient(stops: [.init(color: Theme.bg, location: 0),
                                   .init(color: Theme.bg, location: s / v),
                                   .init(color: clear, location: 1)])
        let inn = Gradient(stops: [.init(color: clear, location: 0),
                                   .init(color: Theme.bg, location: 1 - s / v),
                                   .init(color: Theme.bg, location: 1)])
        // 1. За блоком, к краю экрана, — сплошной фон во всю его ширину.
        ctx.fill(rect(x0, 0, w, straight), with: .color(Theme.bg))
        // 2. Бока: гаснут наружу по горизонтали.
        ctx.fill(rect(0, 0, v, straight), with: .linearGradient(
            inn, startPoint: CGPoint(x: 0, y: 0), endPoint: CGPoint(x: v, y: 0)))
        ctx.fill(rect(x0 + w, 0, v, straight), with: .linearGradient(
            out, startPoint: CGPoint(x: x0 + w, y: 0), endPoint: CGPoint(x: x0 + w + v, y: 0)))
        // 3. Кромка подъезда: гаснет вниз. Только между дугами — по краям её
        //    продолжают углы, иначе на стыке сложились бы два гашения и вышло бы
        //    тёмное пятно.
        ctx.fill(rect(x0 + r, y0 + h, w - 2 * r, v), with: .linearGradient(
            out, startPoint: CGPoint(x: 0, y: y0 + h), endPoint: CGPoint(x: 0, y: y0 + h + v)))
        // 4. ВЫРЕЗЫ УГЛОВ. Снаружи дуги расстояние до контура — радиальное от
        //    центра дуги, поэтому и гашение здесь радиальное. Ровно из-за этого
        //    куска текст больше не торчит из-под скруглений.
        for (cx, left) in [(x0 + r, true), (x0 + w - r, false)] {
            ctx.fill(rect(left ? cx - r - v : cx, y0 + h - r, r + v, r + v),
                     with: .radialGradient(
                        Gradient(colors: [Theme.bg, clear]),
                        center: CGPoint(x: cx, y: y0 + h - r),
                        startRadius: r + s, endRadius: r + v))
        }
    }
    .padding(.horizontal, -veilWidth)
    // Сторона, с которой подъезжает содержимое, — гасим.
    .padding(up ? .top : .bottom, -veilWidth)
    // Сторона края экрана — силуэт уходит за него целиком.
    .padding(up ? .bottom : .top, -haloBack)
    .allowsHitTesting(false)
}

// Ниже плавает нав-бар; он приподнят над краем на 42pt (было 34), поэтому
// отступ подрос на ту же величину — иначе последняя карточка пряталась бы.
// Правило: низ ПОКОЯЩЕГОСЯ содержимого = верх завесы нав-бара, иначе последняя
// карточка гасла бы, стоя на месте. 108 = 104 + 4 при завесе `veilWidth` 18
// (было 114 = 104 + 10 при завесе 24).
extension View { func navPadding() -> some View { self.padding(.bottom, 108) } }
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

// ── ВКЛАДКА «СЕРВЕР» ─────────────────────────────────────────────────────────

struct ServerTab: View {
    @EnvironmentObject var app: AppState
    @State private var coordField = ""

    // Кольцо отвечает на «сервер доступен?» — это ДА/НЕТ. Качество связи
    // показывает пинг отдельной плиткой. Раньше кольцо краснело от медленного
    // пинга, и «На связи» с красной точкой читалось как поломка.
    // ОБРЫВ СВЯЗИ — НЕ БЕДА, а янтарь. Он чинится сам за секунды, и кричать о
    // нём красным (кружок, значок, рамка вокруг самого большого блока экрана,
    // точка на нав-баре — четырьмя способами разом) значит расходовать цвет
    // тревоги на то, что пройдёт само. Красный остаётся там, где само не
    // пройдёт: ошибка настроек и ошибка подключения.
    private var tint: Color {
        app.serverOnline == true ? Theme.accent : Theme.amber
    }
    private var icon: String {
        app.serverOnline == false ? "antenna.radiowaves.left.and.right.slash" : "antenna.radiowaves.left.and.right"
    }
    private var statusText: String {
        switch app.serverOnline { case .some(true): return "На связи"; case .some(false): return "Нет связи — восстанавливаю…"; default: return "Проверяю связь…" }
    }
    /// Та же линейка, что у пинга до хоста (см. `PingTile`).
    private var pingColor: Color {
        app.ping < 250 ? Theme.fg : (app.ping <= 500 ? Theme.amber : Theme.red)
    }
    private var addr: String { app.coordinator.replacingOccurrences(of: "https://", with: "").replacingOccurrences(of: "http://", with: "") }

    var body: some View {
        // Панель — НАЛОЖЕНИЕ поверх прокрутки: `safeAreaInset` заводит её выше
        // содержимого и ровно на её высоту отодвигает начало прокрутки, а само
        // содержимое продолжает ездить ПОД ней. Соседом в `VStack` панель
        // занимала непрозрачную полосу во всю ширину, и список обрывался о её
        // нижний край.
        scrollBody
            .safeAreaInset(edge: .top, spacing: 0) { hero }
            // Открыли вкладку — сразу свежая цифра, не дожидаясь очередного круга.
            // Сам цикл живёт всегда (см. watchServer), как на десктопе.
            .onAppear { coordField = addr; app.checkServer() }
    }

    private var scrollBody: some View {
        ScrollView(showsIndicators: false) {
            VStack(alignment: .leading, spacing: 14) {
                Text("Сервер только помогает найти хостов и связаться с ними. Ваш трафик идёт напрямую к хосту, мимо сервера.")
                    .foregroundColor(Theme.dim).font(.system(size: 12))
                    .frame(maxWidth: .infinity, alignment: .leading)

                sectionLabel("Другой адрес сервера")
                TextField("адрес сервера", text: $coordField)
                    .foregroundColor(Theme.fg).autocorrectionDisabled().textInputAutocapitalization(.never)
                    .padding(14).background(Theme.card).cornerRadius(14)

                Button { app.saveCoordinator(coordField.isEmpty ? app.coordinator : coordField) } label: { calmButton("Сохранить и проверить") }
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
                                    // Тот же чип, что и «Недавние» на вкладке VPN: на s1.
                                    .padding(.horizontal, 14).padding(.vertical, 9).background(Theme.card).cornerRadius(16)
                                    .onTapGesture { coordField = url; app.saveCoordinator(url) }
                            }
                        }.padding(.horizontal, 1)
                    }
                }
            }
            // Сверху 10, а не 22: 12 из прежних 22 переехали в обёртку панели
            // (см. PinnedPanel) — там они дают ореолу место, которое
            // `safeAreaInset` иначе отрезает. Расстояние от кромки панели до
            // покоящегося содержимого осталось прежним, 34.
            .padding(.horizontal, 20).padding(.top, 10).padding(.bottom, 20).navPadding()
        }
    }

    /// Прижатая панель. Когда связь есть, круг уступает место цифрам: смотреть
    /// на большой значок «всё хорошо» смысла нет.
    private var hero: some View {
        PinnedPanel(tint: tint) {
            // Ветка — одна вьюха, меняется мгновенно; едет только высота панели
            // (см. PinnedPanel).
            if app.serverOnline == true {
                VStack(spacing: 8) {
                    StatusLine(icon: icon, title: statusText, tint: tint)
                    Text(addr).foregroundColor(Theme.dim).font(.system(size: 13, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                    HStack(spacing: 8) {
                        // Обычная плитка, а не кнопка: проверка идёт сама каждую секунду
                        // секунды, и нажатие экономило бы в лучшем случае их же.
                        StatTile(label: "ПИНГ", value: "\(app.ping) мс", tint: pingColor)
                        StatTile(label: "ХОСТОВ", value: "\(app.hosts.count)")
                    }
                    CopyTile(label: "ВАШ IP", value: app.myIp.isEmpty ? "—" : app.myIp)
                    // Когда туннель поднят, замер до координатора идёт ИЗНУТРИ него:
                    // это сокет приложения, а приложение с устройства ходит через
                    // свой же VPN. То есть цифра — «туннель + координатор», а не путь
                    // до сервера, о котором человек думает, глядя на слово «ПИНГ».
                    // (Сигналинг при этом идёт сокетом расширения, мимо туннеля, —
                    // тем более незачем выдавать этот замер за путь до сервера.)
                    // Условие именно `TunnelManager.available`: на симуляторе
                    // туннеля нет, поднят только канал, и приписка была бы враньём.
                    // На Android такой подписи нет и не нужно: там приложение
                    // исключено из туннеля целиком, и цифра честная сама по себе.
                    if app.vpnState == 2 && TunnelManager.available {
                        Text("Пинг измерен через туннель — в него входит и путь до хоста")
                            .foregroundColor(Theme.dim.opacity(0.7)).font(.system(size: 11))
                            .multilineTextAlignment(.center)
                            .transition(.opacity)
                    }
                }.transition(.identity)
            } else {
                VStack(spacing: 8) {
                    HeroCircle(icon: icon, tint: tint, pulsing: app.checking)
                    Text(statusText).foregroundColor(Theme.fg).font(.system(size: 21, weight: .heavy))
                        .multilineTextAlignment(.center)
                    Text(addr).foregroundColor(Theme.dim).font(.system(size: 13, design: .monospaced))
                }.transition(.identity)
            }
        }
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
        // Тот же чип, что и везде: невыбранный на s1, выбранный — s1 плюс
        // подкраска. Раньше у него была своя пара чисел (0.16 и рамка 0.5) и
        // своя ступень (s2) — третий рецепт одного и того же.
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(highlighted ? Theme.picked() : Theme.card)
                .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(highlighted ? Theme.edge() : Color.clear, lineWidth: 1))
        )
        .contentShape(Rectangle())
        .onTapGesture { if app.vpnState == 0 { app.connectByCode(id) } }
    }

    var body: some View {
        // Панель — НАЛОЖЕНИЕ поверх прокрутки (см. PinnedPanel): список хостов
        // идёт во всю высоту и уходит ПОД неё.
        scrollBody
            .safeAreaInset(edge: .top, spacing: 0) { VPNHero(inviteCode: $inviteCode) }
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
                            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Theme.cardHi)
                                .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Theme.hairline, lineWidth: 1)))
                    }
                    // Перейти — ТА ЖЕ СТУПЕНЬ, ЧТО У «ВСТАВИТЬ» (s2). Мята отсюда
                    // снята: она обещает «работает/включено», а это просто
                    // переход. Ступень выше соседки тут тоже была лишней — на
                    // одном экране выходило три ступени кнопок; владелец
                    // посмотрел и выбрал одну. Старшинство несут ДРУГИЕ признаки,
                    // и их два: кнопка шире (52 против 44) и глиф в ней светлый
                    // (fg против dim у «вставить»).
                    Button { let c = code; code = ""; app.connectByCode(c) } label: {
                        Image(systemName: "arrow.right").font(.system(size: 21, weight: .bold))
                            .foregroundColor(Theme.fg).frame(width: 52, height: 44)
                            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Theme.cardHi)
                                .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Theme.hairline, lineWidth: 1)))
                    }
                }
                .padding(5).background(Theme.card).cornerRadius(14)

                // «Сканировать QR» — ТА ЖЕ СТУПЕНЬ, ЧТО У ДВУХ КНОПОК КОДА (s2).
                // Стояла на s3 и была самым светлым пятном страницы, хотя это не
                // главное действие экрана. Ступень s1 («на ступень выше своей
                // подложки», а подложка здесь — страница) не годится: рядом лежит
                // поле кода, оно ровно на s1, и кнопка слилась бы с ним в одно
                // пятно. Один экран — одна ступень для кнопки.
                Button { showScanner = true } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "qrcode.viewfinder").font(.system(size: 16, weight: .bold)).foregroundColor(Theme.accent)
                        Text("Сканировать QR").font(.system(size: 15, weight: .bold)).foregroundColor(.white)
                    }
                    .frame(maxWidth: .infinity).padding(.vertical, 13)
                    .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Theme.cardHi)
                        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(Theme.hairline, lineWidth: 1)))
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
                    Text(app.serverOnline == false ? "Нет связи с сервером — восстанавливаю.\nЕсли надолго, проверьте адрес во вкладке «Сервер»." : "Хостов пока нет.\nВведите код сети или поднимите свой во вкладке «Хост».")
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
            // Сверху 10, а не 22: 12 из прежних 22 переехали в обёртку панели
            // (см. PinnedPanel) — там они дают ореолу место, которое
            // `safeAreaInset` иначе отрезает. Расстояние от кромки панели до
            // покоящегося содержимого осталось прежним, 34.
            .padding(.horizontal, 20).padding(.top, 10).padding(.bottom, 20).navPadding()
        }
    }
}

// ── Единый блок статуса: выключен → подключаюсь → подключено ──────────────────
// Один и тот же блок перетекает между состояниями (кольцо, значок, цвет),
// чтобы взгляд не искал статус в новом месте.

struct VPNHero: View {
    @EnvironmentObject var app: AppState
    @Binding var inviteCode: String?

    private var tint: Color {
        // ЦВЕТ СОСТОЯНИЯ, А НЕ ЦВЕТ ЭКРАНА. Выключено — dim: раньше здесь стоял
        // акцент, и после слияния зелёного с мятой кольцо панели горело бы одной
        // мятой и при «VPN выключен», и при «Подключено» — то есть отвечало бы
        // одинаково на единственный вопрос, ради которого сюда смотрят.
        switch app.vpnState { case 1: return Theme.amber; case 2: return Theme.accent; default: return Theme.dim }
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
            // ВЕТКА — ОДНА ВЬЮХА, И МЕНЯЕТСЯ ОНА МГНОВЕННО. Своя стопка у каждой
            // ветки нужна ровно для этого: `.transition` на голом `if` разошёлся
            // бы по всем его детям поодиночке, и на время перехода в панели
            // лежали бы обе разметки сразу. Плавность несёт высота панели
            // (см. PinnedPanel), а текст никуда не едет — он просто другой.
            if app.vpnState == 2 {
                // Подключено — круг уступает место пользе: сколько идёт, куда,
                // адрес хоста, гости и чем позвать друзей.
                VStack(spacing: 8) {
                    TimelineView(.periodic(from: .now, by: 1)) { _ in
                        StatusLine(icon: "checkmark.shield.fill", title: title,
                                   clock: uptimeText(app.connectedSince), tint: tint)
                    }
                    subtitle.foregroundColor(Theme.dim).font(.system(size: 13))
                        .frame(maxWidth: .infinity, alignment: .leading)
                    // Плитки и кнопки внутри ветки появляются по-настоящему
                    // (хост опознался, код пришёл) — им РАСТВОРЕНИЕ НА МЕСТЕ,
                    // а не переход по умолчанию.
                    if let h = host {
                        HStack(spacing: 8) {
                            CopyTile(label: "IP ХОСТА", value: h.ip.isEmpty ? "—" : h.ip)
                            StatTile(label: "ГОСТЕЙ", value: "\(h.guests) / \(h.max)", symbol: "person.2.fill")
                        }.transition(.opacity)
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
                }.transition(.identity)
            } else {
                // Показывать нечего, кроме статуса — круг честно занимает место.
                VStack(spacing: 8) {
                    HeroCircle(icon: icon, tint: tint, pulsing: app.vpnState == 1)
                    Text(title).foregroundColor(Theme.fg).font(.system(size: 21, weight: .heavy))
                        .lineLimit(1).minimumScaleFactor(0.6)
                    subtitle.foregroundColor(Theme.dim).font(.system(size: 13))
                        .multilineTextAlignment(.center)
                    // Разовое сообщение об отказе: отдельно от vpnState, иначе фоновый
                    // опрос статуса ядра его затирает (или оставляет навсегда).
                    // Красный — только для настоящего отказа. Штатно завершённая
                    // раздача идёт тем же приглушённым цветом, что и строка над ней:
                    // ошибки нет, объяснять нужно спокойно.
                    if let err = app.vpnError {
                        Text(err).foregroundColor(app.vpnNoticeCalm ? Theme.dim : Theme.red).font(.system(size: 13))
                            .multilineTextAlignment(.center)
                            .transition(.opacity)
                    }
                }.transition(.identity)
            }
        }
    }
}

struct IdentifiedString: Identifiable { let value: String; var id: String { value }; init(_ v: String) { value = v } }

struct HostCard: View {
    @EnvironmentObject var app: AppState
    let host: Host
    @State private var password = ""
    private var expanded: Bool { app.expandedId == host.id }
    /// Заливка плиток внутри раскрытой карточки: ЧИСТАЯ СТУПЕНЬ s3, БЕЗ ПОДКРАСКИ.
    ///
    /// Мята с плиток снята: в раскрытой карточке их семь, и семь подкрашенных
    /// ячеек читались как «всё это включено», хотя это просто цифры о хосте.
    /// Ступень s3 для того и заведена — она отличима от своей подложки (L* 7.71
    /// над s2 раскрытой карточки), но остаётся нейтральной. Та же ступень лежит
    /// под плитками панели состояния, так что по оттенку они одно и то же.
    private var tileFill: Color { Theme.tile }

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
                        // ЗНАЧОК ПРОТОКОЛА — ПЕРЕД ИМЕНЕМ, в одной строке с ним.
                        //
                        // Значок есть у КАЖДОГО протокола: раньше он был только
                        // у «Маскировки», и его отсутствие означало сразу две
                        // противоположные вещи — «Обычный» (шифр есть) и «Без
                        // шифра» (шифра нет); незащищённый хост выглядел ровно
                        // как защищённый. Стоял он в подписи, под именем; теперь
                        // рядом с самим именем — защита относится к хосту, а не
                        // к счётчику гостей.
                        //
                        // Первым, а не в хвосте: хвост съедает многоточие, и
                        // знак защиты пропадал бы ровно там, где строке не
                        // хватило ширины. Ужимается имя (lineLimit(1)), значок
                        // фиксированный — выдавить его нечем.
                        HStack(spacing: 5) {
                            // Коробка значка ФИКСИРОВАННОЙ ширины (`protoBox`) —
                            // ровно ради этого столбца: у SF Symbols ширина своя
                            // у каждого знака, и имена начинались с разных мест.
                            Image(systemName: protoIcon(host.proto))
                                .foregroundColor(Theme.dim).font(.system(size: 12, weight: .semibold))
                                .frame(width: protoBox)
                            Text(host.name.isEmpty ? host.id : host.name)
                                .foregroundColor(Theme.fg).font(.system(size: 16, weight: .semibold))
                                .lineLimit(1).minimumScaleFactor(0.8)
                        }
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
                        CopyTile(label: "КОД", value: host.id, fill: tileFill)
                        CopyTile(label: "IP", value: host.ip.isEmpty ? "—" : host.ip, fill: tileFill)
                    }
                    HStack(spacing: 8) {
                        StatTile(label: "СТРАНА", value: countryLabel(host), fill: tileFill)
                        StatTile(label: "ГОСТЕЙ", value: "\(host.guests) / \(host.max)", fill: tileFill)
                        PingTile(value: app.pings[host.id] ?? "…", fill: tileFill)
                    }
                    HStack(spacing: 8) {
                        StatTile(label: "ДОСТУП", value: host.hasPassword ? "по паролю" : "открытый",
                                 symbol: host.hasPassword ? "lock.fill" : "lock.open.fill", fill: tileFill)
                        StatTile(label: "ПРОТОКОЛ", value: protoName(host.proto),
                                 symbol: protoIcon(host.proto), fill: tileFill)
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
        // ВНУТРИ КАРТОЧКИ МЯТЫ НЕТ — ОДНИ СТУПЕНИ.
        //
        // Пробовали и так, и эдак: сперва мятой красили карточку целиком (цвет
        // доставался оболочке, а не содержимому), потом — плитки внутри (семь
        // подкрашенных ячеек читались как «всё это включено»). Осталась чистая
        // лестница: свёрнутая карточка s1 (L* 8.1), раскрытая s2 (11.6), плитки
        // внутри неё s3 (19.31). Все три различимы, и ни одна ничего не обещает.
        //
        // «РАСКРЫТА» ЧИТАЕТСЯ БЕЗ ПОДКРАСКИ — ступенью и рамкой; рамка осталась
        // мятной (edgeSoft), так что признак не один.
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(expanded ? Theme.cardHi : Theme.card)
                .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(expanded ? Theme.edgeSoft() : Theme.hairline, lineWidth: 1))
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
    ///
    /// ПЛАШКА НА СТУПЕНЬ НИЖЕ ПЛИТКИ (s2, а не s3), САМ ФЛАГ ПРИГЛУШЁН. Флаг —
    /// единственное полностью насыщенное пятно на экране, и сидел он на самой
    /// светлой подложке темы: в списке глаз ловил его раньше имени хоста, ради
    /// которого на строку и смотрят.
    ///
    /// СТУПЕНЬ СЧИТАЕТСЯ ОТ СВОЕЙ КАРТОЧКИ, А НЕ ВПИСАНА ЧИСЛОМ. Жёсткий s2
    /// совпадал с фоном РАСКРЫТОЙ карточки — плашка пропадала
    /// целиком, от неё оставалась одна рамка.
    private var flagAvatar: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 14, style: .continuous).fill(expanded ? Theme.tile : Theme.cardHi)
            RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(Theme.hairline, lineWidth: 1)
            Text(hostFlag(host)).font(.system(size: 29)).opacity(0.85)
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
                        Capsule().fill(frac < 0.8 ? Theme.accent : Theme.amber)
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
        if app.hosting { return Theme.accent }
        // Выключено — dim, а не акцент: иначе «Раздача выключена» и «Раздаю»
        // носили бы одну мяту.
        return app.hostError != nil ? Theme.red : Theme.dim
    }
    private var statusTitle: String {
        if app.starting { return "Запускаюсь…" }
        if app.hosting { return "Раздаю" }
        return app.hostError != nil ? "Не удалось начать раздачу" : "Раздача выключена"
    }

    var body: some View {
        // Панель — НАЛОЖЕНИЕ поверх прокрутки (см. PinnedPanel): настройки
        // идут во всю высоту и уходят ПОД неё.
        scrollBody
            .safeAreaInset(edge: .top, spacing: 0) { hero }
            .onAppear { app.ensureHostCode() }
            .sheet(item: Binding(get: { qrCode.map { IdentifiedString($0) } }, set: { qrCode = $0?.value })) { item in
                QRSheet(code: item.value)
            }
    }

    /// Прижатая панель хост-режима — тот же язык, что у VPN и сервера.
    /// Код сети НЕ показывается, пока раздача выключена: давать его некому.
    private var hero: some View {
        PinnedPanel(tint: tint) {
            // Ветка — одна вьюха, меняется мгновенно; едет только высота панели
            // (см. PinnedPanel).
            if app.hosting {
                VStack(spacing: 8) {
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
                    // Поделиться — в НИЗУ панели, прямо под самим кодом. Код
                    // приходит с задержкой, поэтому кнопки честно появляются —
                    // растворением НА МЕСТЕ.
                    if !app.hostCode.isEmpty {
                        ShareButtons(code: app.hostCode, qrCode: $qrCode).padding(.top, 2)
                            .transition(.opacity)
                    }
                }.transition(.identity)
            } else {
                VStack(spacing: 8) {
                    HeroCircle(icon: "wifi.router.fill", tint: tint, pulsing: app.starting)
                    Text(statusTitle).foregroundColor(Theme.fg).font(.system(size: 21, weight: .heavy))
                        .multilineTextAlignment(.center).lineLimit(1).minimumScaleFactor(0.7)
                    Text(app.starting ? "Пробиваю канал наружу…"
                         : (app.hostError ?? "Станьте выходной точкой для друзей"))
                        .foregroundColor(app.hostError != nil && !app.starting ? Theme.red : Theme.dim)
                        .font(.system(size: 13)).multilineTextAlignment(.center)
                }.transition(.identity)
            }
        }
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
                     ? "С паролем сеть всегда скрыта: публичная карточка выдала бы её как раз тем, от кого вы закрылись. Подключение — по коду."
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
                // То же правило, что у чипов: выбранное — своя ступень плюс
                // подкраска, плюс рамка и цветная цифра. Невыбранная кнопка
                // теперь на s1, как и чипы видимости выше: раньше эти две
                // соседние группы «невыбранного» сидели на разных ступенях
                // (s2 и s1) — на одном экране, в сорока строках друг от друга.
                HStack(spacing: 6) {
                    ForEach([4, 8, 16, 32, 64, 128], id: \.self) { v in
                        Button { app.hostMax = v; app.applyHostNow() } label: {
                            Text("\(v)").font(.system(size: 13, weight: .bold)).foregroundColor(app.hostMax == v ? Theme.accent : Theme.dim)
                                .frame(maxWidth: .infinity).padding(.vertical, 8)
                                .background(app.hostMax == v ? Theme.picked() : Theme.card)
                                .cornerRadius(10)
                                .overlay(RoundedRectangle(cornerRadius: 10)
                                    .stroke(app.hostMax == v ? Theme.edge() : Color.clear, lineWidth: 1))
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
                Text("Раздача идёт, пока приложение открыто — в фоне система её глушит.")
                    .foregroundColor(Theme.dim).font(.system(size: 12))
            }
            // Сверху 10, а не 22: 12 из прежних 22 переехали в обёртку панели
            // (см. PinnedPanel) — там они дают ореолу место, которое
            // `safeAreaInset` иначе отрезает. Расстояние от кромки панели до
            // покоящегося содержимого осталось прежним, 34.
            .padding(.horizontal, 20).padding(.top, 10).padding(.bottom, 20).navPadding()
        }
    }

    /// Толстый чип: значок сверху, название снизу — палец попадает не глядя.
    ///
    /// Выбранный — общее правило приложения: своя ступень плюс подкраска
    /// (Theme.picked) плюс рамка и цветной текст. Сплошной градиент со свечением
    /// кричал сильнее самого выбора: таких чипов на вкладке подряд шесть, и
    /// экран из них выходил в синих плашках.
    private func bigChip(_ icon: String, _ name: String, on: Bool, _ tap: @escaping () -> Void) -> some View {
        VStack(spacing: 5) {
            Image(systemName: icon).font(.system(size: 18, weight: .semibold)).foregroundColor(on ? Theme.accent : Theme.dim)
            Text(name).font(.system(size: 13, weight: .bold)).foregroundColor(on ? Theme.accent : Theme.dim)
                .lineLimit(1).minimumScaleFactor(0.8)
        }
        .frame(maxWidth: .infinity).padding(.vertical, 14)
        .background(on ? Theme.picked() : Theme.card)
        .cornerRadius(14)
        .overlay(RoundedRectangle(cornerRadius: 14)
            .stroke(on ? Theme.edge() : Theme.hairline, lineWidth: 1))
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
