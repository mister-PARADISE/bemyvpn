import SwiftUI
import UIKit

// ── ЭКРАНЫ ───────────────────────────────────────────────────────────────────
//
// Три вкладки, карточка хоста и лист приглашения. Детали, из которых они
// собраны, живут в Kit.swift, отделка парящих блоков — в Halo.swift.

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
        //
        // РАДИУС НЕ ПЕРЕДАЁТСЯ: он один на оба парящих блока (`floatRadius`).
        // Здесь стояло 28 против 22 у панели, и от этого расходилось ГАШЕНИЕ —
        // его ширина считается от радиуса.
        .background(floatSurface(stroke: Theme.hairlineFloat, up: true))
        .padding(.horizontal, 18)
        // safe-area пробита на корне (ContentView), поэтому зазор снизу задаём
        // руками. Нужно ОДНОВРЕМЕННО: (1) не влезать в зону home-indicator внизу
        // и (2) не «висеть» высоко над краем. 42pt (было 34) — бар заметно
        // приподнят над краем, но не отрывается. Прибавка идёт СВЕРХ безопасной
        // зоны, поэтому на устройствах с индикатором и без него ощущение одно.
        .padding(.bottom, 42)
    }

    /// Радиус внутренней таблетки. Концентричность: внешний R − отступ,
    /// иначе внутренняя форма не следует внешней. Считается, а не вписан:
    /// внешний радиус переехал в `floatRadius` и стал общим с панелью.
    private let innerR: CGFloat = floatRadius - 6

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

// ── ВКЛАДКА «СЕРВЕР» ─────────────────────────────────────────────────────────

struct ServerTab: View {
    @EnvironmentObject var app: AppState
    @State private var coordField = ""

    /// Подпись состояния связи и её тревожность — ПАРОЙ из справочника.
    private var link: (text: String, alarm: Int) { Core.link(app.serverOnline) }

    // Кольцо отвечает на «сервер доступен?» — это ДА/НЕТ. Качество связи
    // показывает пинг отдельной плиткой. Раньше кольцо краснело от медленного
    // пинга, и спокойное состояние с красной точкой читалось как поломка.
    // ОБРЫВ СВЯЗИ — НЕ БЕДА, а янтарь. Он чинится сам за секунды, и кричать о
    // нём красным (кружок, значок, рамка вокруг самого большого блока экрана,
    // точка на нав-баре — четырьмя способами разом) значит расходовать цвет
    // тревоги на то, что пройдёт само. Красный остаётся там, где само не
    // пройдёт: ошибка настроек и ошибка подключения. Тревогу считает справочник
    // (обрыв и «ещё не знаем» — не спокойствие), цвет выбираем свой: мята здесь
    // значит «работает», а такого уровня у тревоги нет и быть не может.
    private var tint: Color { link.alarm == 0 ? Theme.accent : Theme.amber }
    private var icon: String {
        app.serverOnline == false ? "antenna.radiowaves.left.and.right.slash" : "antenna.radiowaves.left.and.right"
    }
    private var addr: String { Core.displayCoordinator(app.coordinator) }

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
                BmvField("адрес сервера", text: $coordField, caps: .never)

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
                                Text(Core.displayCoordinator(url))
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
                    StatusLine(icon: icon, title: link.text, tint: tint)
                    Text(addr).foregroundColor(Theme.dim).font(.system(size: 13, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                    HStack(spacing: 8) {
                        // Обычная плитка, а не кнопка: проверка идёт сама каждую секунду
                        // секунды, и нажатие экономило бы в лучшем случае их же.
                        let p = Core.ping(app.ping)
                        StatTile(label: "ПИНГ", value: p.text, tint: alarmColor(p.alarm))
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
                    Text(link.text).foregroundColor(Theme.fg).font(.system(size: 21, weight: .heavy))
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

    /// Кнопка в строке кода: одна отделка на все три (ступень s2, скругление 10,
    /// высота 44). Различаются они только шириной и краской глифа — этим и несёт
    /// старшинство «перейти».
    ///
    /// `label` — подпись ДЛЯ ОЗВУЧКИ: рядом с глифом нет текста, и без неё
    /// VoiceOver прочитал бы имя системного значка вместо действия.
    private func codeButton(_ icon: String, label: String, width: CGFloat = 44,
                            glyph: CGFloat = 17, weight: Font.Weight = .semibold,
                            tint: Color = Theme.dim, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon).font(.system(size: glyph, weight: weight)).foregroundColor(tint)
                .frame(width: width, height: 44)
                .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Theme.cardHi)
                    .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Theme.hairline, lineWidth: 1)))
        }
        .accessibilityLabel(label)
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

    // `GeometryReader` + `ScrollViewReader` — ради умной прокрутки к раскрытой
    // карточке (см. `HostCard`). Ничего не рисуют и разметку не трогают: первый
    // отдаёт высоту окна прокрутки, второй — саму прокрутку.
    private var scrollBody: some View {
        GeometryReader { vp in
        ScrollViewReader { scroll in
        ScrollView(showsIndicators: false) {
            VStack(alignment: .leading, spacing: 14) {
                sectionLabel("Подключиться по коду")
                HStack(spacing: 6) {
                    BmvField("КОД СЕТИ", text: $code, caps: .characters, boxed: false).padding(12)
                    // ТРИ ДЕЙСТВИЯ НАД ОДНИМ И ТЕМ ЖЕ — В ОДНОЙ СТРОКЕ. «Снять код
                    // камерой» стояло отдельной широкой кнопкой под полем, хотя это
                    // такой же способ НАПОЛНИТЬ поле, как «вставить», — и занимало
                    // целую строку экрана.
                    //
                    // Порядок: редкое слева, завершающее справа, у большого пальца.
                    // Скан нужен реже всех (требует второго экрана рядом), поэтому
                    // он крайний слева — и обе прежние кнопки остались там же, где
                    // к ним привыкли. Краска глифа у скана `dim`, как у «вставить»:
                    // это два равных способа наполнить поле, а мята обещала бы
                    // «работает/включено» (по той же причине её сняли с «перейти»).
                    codeButton("qrcode.viewfinder", label: "Сканировать QR") { showScanner = true }
                    codeButton("doc.on.clipboard", label: "Вставить код") {
                        if let s = UIPasteboard.general.string { code = s }
                    }
                    // Перейти — ТА ЖЕ СТУПЕНЬ, ЧТО У «ВСТАВИТЬ» (s2). Мята отсюда
                    // снята: она обещает «работает/включено», а это просто
                    // переход. Ступень выше соседки тут тоже была лишней — на
                    // одном экране выходило три ступени кнопок; владелец
                    // посмотрел и выбрал одну. Старшинство несут ДРУГИЕ признаки,
                    // и их два: кнопка шире (52 против 44) и глиф в ней светлый
                    // (fg против dim у двух соседок).
                    codeButton("arrow.right", label: "Подключиться по коду",
                               width: 52, glyph: 21, weight: .bold, tint: Theme.fg) {
                        let c = code; code = ""; app.connectByCode(c)
                    }
                }
                // СТРОКА КОДА — ЭТО САМО ПОЛЕ, А НЕ КОРОБКА С ПОЛЕМ ВНУТРИ: текст
                // лежит прямо на этой заливке, кнопки просто припаркованы в правом
                // её конце. Поэтому кромка идёт по внешнему контуру строки — там,
                // где на самом деле кончается поле; обвести вместо этого внутренний
                // участок значило бы провести линию посреди сплошной заливки одного
                // цвета.
                //
                // КНОПКИ ВНУТРИ ОСТАЮТСЯ СИНИМИ, И ЭТО НЕ ИСКЛЮЧЕНИЕ ИЗ ПРАВИЛА, А
                // ЕГО ПРИЧИНА. Белую волосяную получает то, что заливка родителя УЖЕ
                // отделила: плитка на парящей панели отстоит от неё на 8.21 по L*,
                // кромке там нечего держать. Здесь перепад s1→s2 всего 2.42 —
                // заливка не отделяет ничего, и кромка кнопки остаётся несущей.
                .padding(5)
                .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Theme.card))
                .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .stroke(Theme.hairline, lineWidth: 1))

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
                    // Пусто по двум разным причинам, и человеку нужно разное:
                    // ждать связи или действовать. Что именно написать — решает
                    // справочник.
                    Text(Core.emptyDirectoryHint(app.serverOnline))
                        .foregroundColor(Theme.dim).font(.system(size: 14)).multilineTextAlignment(.center).frame(maxWidth: .infinity).padding(.vertical, 40)
                } else {
                    // Данные не живые — показываем это САМИМ СПИСКОМ, а не ещё
                    // одним элементом: приглушённое читается как «неактуально»
                    // мгновенно и без слов.
                    ForEach(shown) { HostCard(host: $0, scroll: scroll, viewportH: vp.size.height) }
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
    /// Заголовок состояния — подпись из справочника (`app.vpnText`). Толковать
    /// числа самим больше не надо: авто-реконнект от первого подключения там
    /// уже отличён.
    ///
    /// Исключение ровно одно: когда известно, К КОМУ подключены, на этом месте
    /// полезнее имя хоста — слово «Подключено» и так видно по цвету.
    private var title: String {
        app.vpnState == 2 ? (host?.name ?? app.connectedTo ?? app.vpnText) : app.vpnText
    }
    // Text, а не String: в подпись вклеивается SF Symbol протокола.
    private var subtitle: Text {
        switch app.vpnState {
        case 1: return Text(host?.name ?? "Пробиваю канал к хосту")
        case 2:
            guard let h = host else { return Text("Канал поднят") }
            return Text(countryLabel(h)) + Text("  ·  ") + symbolText(protectionIcon(h.protection), h.protoName)
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


struct HostCard: View {
    @EnvironmentObject var app: AppState
    let host: Host
    /// Прокрутка списка — только чтобы подтянуть себя, когда раскрылись.
    let scroll: ScrollViewProxy
    /// Высота окна прокрутки: из неё считается доля привязки (см. ниже).
    let viewportH: CGFloat
    @State private var password = ""
    /// Своя высота по замеру: по ней (а НЕ по часам) ждём конца раскрытия.
    @State private var cardH: CGFloat = 0
    private var expanded: Bool { app.expandedId == host.id }
    /// МЫ СЕЙЧАС В ЭТОЙ СЕТИ. Не «раскрыта» и не «выбрана» — это разные вещи:
    /// раскрыта может быть одна карточка, а работает соединение с другой.
    ///
    /// `vpnState == 2` ОБЯЗАТЕЛЬНО, а не один `connectedTo`: последний ставится
    /// оптимистично, ещё на «Подключаюсь» (`AppState.connect`), и метить им
    /// карточку значило бы обещать работающую сеть до того, как она встала.
    private var live: Bool { app.vpnState == 2 && app.connectedTo == host.id }
    /// Заливка плиток внутри раскрытой карточки: ЧИСТАЯ СТУПЕНЬ s3, БЕЗ ПОДКРАСКИ.
    ///
    /// Мята с плиток снята: в раскрытой карточке их семь, и семь подкрашенных
    /// ячеек читались как «всё это включено», хотя это просто цифры о хосте.
    /// Ступень s3 для того и заведена — она отличима от своей подложки (L* 10.25
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
                            Image(systemName: protectionIcon(host.protection))
                                .foregroundColor(Theme.dim).font(.system(size: 12, weight: .semibold))
                                .frame(width: protoBox)
                            Text(host.name.isEmpty ? host.id : host.name)
                                .foregroundColor(Theme.fg).font(.system(size: 16, weight: .semibold))
                                .lineLimit(1).minimumScaleFactor(0.8)
                            // Чип «Подключено» — В СТРОКЕ ИМЕНИ, а не отдельной
                            // полосой: он говорит про сам хост, и читать его
                            // надо там же, где читают имя. Ширина у чипа
                            // фиксированная, у имени сжимаемая — уступает имя.
                            if live { StateChip(text: "Подключено", tint: Theme.accent) }
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
                        PingTile(ping: app.pingOf[host.id] ?? .measuring, fill: tileFill)
                    }
                    HStack(spacing: 8) {
                        StatTile(label: "ДОСТУП", value: host.hasPassword ? "по паролю" : "открытый",
                                 symbol: host.hasPassword ? "lock.fill" : "lock.open.fill", fill: tileFill)
                        StatTile(label: "ПРОТОКОЛ", value: host.protoName,
                                 symbol: protectionIcon(host.protection), fill: tileFill)
                    }
                }

                if host.hasPassword {
                    // Кромка БЕЛАЯ: поле лежит ВНУТРИ раскрытой карточки, рядом с
                    // плитками, и берёт их волосяную (см. «не везде» в Theme.swift).
                    BmvField("Пароль", text: $password, secure: true, boxed: false)
                        .padding(12)
                        .background(RoundedRectangle(cornerRadius: 11, style: .continuous).fill(Theme.tile)
                            .overlay(RoundedRectangle(cornerRadius: 11, style: .continuous).stroke(Theme.hairlineInner, lineWidth: 1)))
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
        // лестница: свёрнутая карточка s1 (L* 6.64), раскрытая s2 (9.06), плитки
        // внутри неё s3 (19.31). Все три различимы, и ни одна ничего не обещает.
        //
        // «РАСКРЫТА» ЧИТАЕТСЯ БЕЗ ПОДКРАСКИ — ступенью и рамкой; рамка осталась
        // мятной (edgeSoft), так что признак не один.
        //
        // ── «ПОДКЛЮЧЕНО» ГОВОРИТ СВЕЧЕНИЕ ВОКРУГ, А НЕ КРОМКА И НЕ ЗАЛИВКА ──
        //
        // Заливка уже занята плотно: ступень отвечает на «раскрыта ли»,
        // подкраска (picked) — на «выбрано ли». Кромка занята тоже: мятая
        // edgeSoft означает «раскрыта». Подключён при этом может быть ОДИН
        // хост, а раскрыт совсем ДРУГОЙ, и увидеть надо оба сразу.
        //
        // ЗДЕСЬ БЫЛА ОБВОДКА — мята в полную силу и вдвое толще. Владелец
        // посмотрел и сказал: «всмысле обводка а не свечение?» — и он прав:
        // обводка спорила с кромкой «раскрыта» тем же языком, только громче.
        // Свечение живёт СНАРУЖИ карточки, где не занято ничего (см. cardGlow).
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
        .animation(.easeInOut(duration: 0.2), value: live)
        // Обрезка по скруглению: пока карточка растёт, содержимое не должно
        // вылезать за её край.
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        // СВЕЧЕНИЕ — ПОСЛЕ обрезки, иначе она срезала бы его целиком: всё, что
        // светится, лежит СНАРУЖИ карточки. Накладкой, а не подложкой: кольца
        // внутри контура не рисуют ни точки, поэтому лежать под карточкой им не
        // обязательно, а накладка не спорит с измерителем высоты ниже.
        .overlay { if live { cardGlow } }
        // ЗАМЕР СВОЕЙ ВЫСОТЫ — не ради разметки, а ради ожидания ниже.
        .background(GeometryReader { g in
            Color.clear
                .onAppear { cardH = g.size.height }
                .onChange(of: g.size.height) { cardH = $0 }
        })
        // РАСКРЫЛИ — ПОДТЯНУТЬ КАРТОЧКУ В ВИДИМОЕ. Прокрутка сама за выросшей
        // карточкой не идёт: у последней в списке всё, что ниже заголовка,
        // уезжало под нав-бар, и «Подключить» человек не видел вовсе.
        //
        // ЖДЁМ ПО САМОЙ ВЫСОТЕ, А НЕ ПО ЧАСАМ. Раскрытие — ДВЕ анимации подряд:
        // коробка растёт (0.2с), содержимое проявляется (0.06 + 0.22с).
        // Отмеренные миллисекунды ловят карточку на середине роста, и прокрутка
        // подтягивается к промежуточному размеру. Ключ задачи включает
        // ЗАМЕРЕННУЮ высоту: тронулась дальше — ожидание начинается заново,
        // замерла на 80мс — двигаем один раз и уже по готовой высоте.
        //
        // Ключ — состояние `app.expandedId`, а не жест: карточку раскрывает ещё
        // и ввод кода, и разбор QR (`connectByCode`), и те пути тапа не знают.
        .task(id: "\(expanded)|\(Int(cardH))") {
            guard expanded, cardH > 0, viewportH > 0 else { return }
            try? await Task.sleep(nanoseconds: 80_000_000)
            guard !Task.isCancelled else { return }
            withAnimation(.easeInOut(duration: 0.25)) {
                scroll.scrollTo(host.id, anchor: UnitPoint(x: 0, y: revealAnchor))
            }
        }
    }

    /// ПОКАЗАТЬ РАСКРЫТУЮ КАРТОЧКУ ЦЕЛИКОМ. Одно правило на всю оболочку — и то
    /// же самое, что в окне и на Android.
    ///
    /// ВИДИМАЯ ПОЛОСА — ЭТО НЕ ЭКРАН, НО СВЕРХУ ЕЁ РЕЗАТЬ НЕ НАДО. Панель
    /// подвешена через `safeAreaInset`, и окно прокрутки НАЧИНАЕТСЯ уже под ней:
    /// замер на симуляторе — экран 874, панель со своей завесой 279, а
    /// `viewportH` = 595, ровно разница. Снизу же нав-бар просто лежит поверх, и
    /// его клиренс (`navClearance`) из высоты никто не вычитал: карточка,
    /// упёршаяся низом в край экрана, «видима» только на бумаге — на деле она под
    /// баром, на этом уже обжигались.
    ///
    /// Помещается в полосу — ЦЕНТРИРУЕМ: глазу не надо искать, куда уехала
    /// карточка. Не помещается — центрировать НЕЛЬЗЯ, спрячется её начало вместе
    /// с именем хоста; тогда прижимаем ВЕРХ карточки к верху полосы (доля 0).
    ///
    /// Считаем ДОЛЕЙ, потому что своей прокрутки «на столько-то точек» у SwiftUI
    /// (iOS 16) нет — только привязка. Доля `k` ставит точку на `k` высоты
    /// карточки в точку на `k` высоты окна, значит верх карточки встаёт на
    /// `k·(V−H)`; отсюда `k = нужный верх / (V−H)`.
    private var revealAnchor: CGFloat {
        let band = viewportH - navClearance
        let want = cardH <= band ? (band - cardH) / 2 : 0
        // Карточка ровно в рост окна: сдвинуть её привязкой нельзя вовсе
        // (`V−H` = 0), так что просим верх — прокрутка сама зажмёт по краям.
        return abs(viewportH - cardH) < 1 ? 0 : want / (viewportH - cardH)
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
            // Кромка БЕЛАЯ: плашка лежит ВНУТРИ карточки, а не на странице
            // (см. «не везде» в Theme.swift).
            RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(Theme.hairlineInner, lineWidth: 1)
            Text(hostFlag(host)).font(.system(size: 29)).opacity(0.85)
        }
        .frame(width: 56, height: 56)
    }

    /// СВЕЧЕНИЕ ПОДКЛЮЧЁННОЙ КАРТОЧКИ: МЯГКОЕ СИЯНИЕ НАРУЖУ, А НЕ ВТОРАЯ ОБВОДКА.
    ///
    /// НЕ ПАЧКАЕТ КАРТОЧКУ, ХОТЬ И ЛЕЖИТ НАКЛАДКОЙ: каждая ступень — не заливка,
    /// а КОЛЬЦО (`strokeBorder` рисует внутрь фигуры) шириной ровно в свой вылет
    /// при скруглении `16 + d`, то есть полоса от контура карточки наружу на d.
    /// Внутри контура не кладётся ни точки.
    ///
    /// МЕХАНИЗМ — ЛЕСЕНКА РАЗДУТЫХ КОНТУРОВ, ТЕМ ЖЕ ПРИЁМОМ, ЧТО И ОРЕОЛ ПАРЯЩЕГО
    /// СЛОЯ. Смещение контура наружу на d превращает скругление R в R + d, то
    /// есть раздутый прямоугольник — ТОЧНАЯ линия равного расстояния до контура:
    /// одинаковая сверху, снизу, сбоку и в вырезах углов.
    ///
    /// РОДНОЙ `.shadow` ЗДЕСЬ НЕ ГОДИТСЯ, И ЭТО ЗАМЕР, А НЕ ВКУС. `radius` у
    /// SwiftUI, `drop-shadow-blur` у Slint и `elevation` у Compose — РАЗНЫЕ
    /// величины: на тени три одинаковых числа дали вылет 13.3 pt против 8.5 px в
    /// окне. Лесенка от единиц оболочки не зависит вовсе.
    ///
    /// ПОЛОСЫ НЕ НАКЛАДЫВАЮТСЯ, И ЭТО ГЛАВНОЕ. Каждая точка красится РОВНО ОДИН
    /// РАЗ, сразу нужной непрозрачностью `peak·(1 − d/reach)`. Первый заход
    /// складывал двадцать четыре полупрозрачных кольца друг на друга —
    /// математически то же самое, а на экране НЕТ: у каждого кольца выходила
    /// альфа 0.009, это 2.3 из 255, и оболочки округляли её по-разному. Замер: у
    /// контура 57 по зелёному здесь и в окне против 63 на Android — расхождение
    /// 12%, ровно цена одной единицы округления, помноженной на двадцать четыре
    /// наложения. Одно наложение вместо двадцати четырёх убирает её целиком.
    ///
    /// СТЫКОВ МЕЖДУ ПОЛОСАМИ НЕ ВИДНО, И ЭТО НЕ ВЕЗЕНИЕ: сглаживание делит
    /// пограничную точку между соседними полосами по покрытию, а покрытия дают в
    /// сумме единицу — выходит ровно та же альфа, что и посередине между ними.
    ///
    /// ПОЧЕМУ СВЕТ МОЖНО ТАМ, ГДЕ ТЕНЬ БЫЛА НЕЛЬЗЯ. Страница `#08090B` — 8/255,
    /// до чистого чёрного восемь единиц: тёмному пятну падать некуда, тень и
    /// давала 2 единицы. У света ход в двадцать семь раз больше — мята по
    /// зелёному каналу 224 против 9 у фона.
    /// РИСУЕТСЯ В `Canvas`, А НЕ СТОПКОЙ ФИГУР С ОТРИЦАТЕЛЬНЫМИ ОТСТУПАМИ. Стопка
    /// здесь стояла и дала ПОЛОСАТОСТЬ: полоса спада 0.5 pt, а раскладка SwiftUI
    /// округляет рамку до целых, и вылеты 0.5 / 1.0 / 1.5 съезжали на общие
    /// границы — каждая третья точка на снимке оставалась чистым фоном (замер:
    /// (8,9,11) через две на третью по всему спаду). У холста координаты дробные
    /// и раскладке не подчиняются. Тем же холстом собран и ореол парящего слоя.
    private var cardGlow: some View {
        // Полос в спаде. 24 на вылет 12 — по полпункта на полосу, меньше точки на
        // любом экране, поэтому лесенки не видно: перепад альфы на полосу
        // (0.22/24) даёт по зелёному каналу две единицы из 255.
        let n = 24
        return Canvas { ctx, size in
            let reach = Theme.glowReach
            let band = reach / CGFloat(n)
            // Коробка самой карточки внутри холста: холст раздут на вылет.
            let w = size.width - 2 * reach, h = size.height - 2 * reach
            for i in 0..<n {
                // Середина полосы: по ней считается и её место, и её
                // непрозрачность. `stroke` ведёт линию по СЕРЕДИНЕ пути, поэтому
                // полоса ложится ровно на [mid − band/2, mid + band/2].
                let mid = band * (CGFloat(i) + 0.5)
                let box = CGRect(x: reach - mid, y: reach - mid, width: w + 2 * mid, height: h + 2 * mid)
                ctx.stroke(
                    Path(roundedRect: box, cornerRadius: 16 + mid, style: .continuous),
                    with: .color(Theme.accent.opacity(Theme.glowPeak * (1 - (Double(i) + 0.5) / Double(n)))),
                    lineWidth: band)
            }
        }
        .padding(-Theme.glowReach)
        .allowsHitTesting(false)
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
    /// Три протокола на выбор: слово и значок — из справочника через мост.
    /// `static` затем, что список один на все вьюхи и считается один раз: вьюха
    /// пересобирается на каждое изменение состояния, а ответы моста постоянны.
    private static let protos: [(String, String, String)] = ["noise", "noise-obfs", "plain"]
        .map { ($0, protectionIcon(Core.protection($0)), Core.protoName($0)) }

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
                        StatTile(label: "ПРОТОКОЛ", value: Core.protoName(app.hostProtocol),
                                 symbol: protectionIcon(Core.protection(app.hostProtocol)))
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
                BmvField("Имя", text: $app.hostName)
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
                BmvField("без пароля", text: $app.hostPassword)
                    .onChange(of: app.hostPassword) { _ in app.applyHostDebounced() }

                sectionLabel("Протокол")
                HStack(spacing: 8) {
                    ForEach(Self.protos, id: \.0) { pid, icon, name in
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
