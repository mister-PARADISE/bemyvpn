# Contributing to BeMyVPN

Thanks for looking. This document is short on purpose.

*(Русская версия — [ниже](#-по-русски).)*

## The single most useful contribution

**Run a host.** Really.

BeMyVPN's whole model is that the exit node is a volunteer's machine, and the
directory is currently very thin. Someone who needs access opens the app, sees an
almost empty list, and leaves for good. No amount of code fixes that — running
hosts do.

If you have a VPS with idle bandwidth:

```bash
curl -fsSL https://raw.githubusercontent.com/mister-PARADISE/bemyvpn/main/install.sh | sh
sudo bemyvpn host --tunnel --autostart
```

It survives logout, SSH drops and reboots. `--hidden` keeps you out of the public
list if you'd rather share only with people you choose.

## Good places to start with code

Issues are labelled with difficulty so you can pick by appetite:

| Label | Meaning |
|---|---|
| `D-easy` | self-contained, no deep knowledge of the codebase needed |
| `D-medium` | touches one subsystem; read its module docs first |
| `D-hard` | protocol, crypto or TCP internals — expect discussion before coding |

Everything tagged [`good first issue`](https://github.com/mister-PARADISE/bemyvpn/labels/good%20first%20issue)
is genuinely scoped, not a chore dump. The highest-impact one right now is
[internationalisation (#1)](https://github.com/mister-PARADISE/bemyvpn/issues/1) —
the UI speaks Russian only, and that is the project's single biggest limit on who
can use it.

## Building

Rust 1.85+.

```bash
cargo build --release -p bmv-cli   # terminal: client + host + coordinator + TUI
cargo build --release -p bmv-gui   # desktop GUI (Slint)
```

Tests run in **three passes**, and this is not an oversight — the coordinator and
the `ipstack` fork live outside the workspace deliberately (they carry their own
dependencies), so `--workspace` does not see them and silently skips them:

```bash
cargo test --workspace
cargo test --manifest-path server/coordinator/Cargo.toml
cargo test --manifest-path vendor/ipstack/Cargo.toml
```

A fourth, the `wintun` fork, only builds on Windows and is tested there. CI runs
all of them; please run at least the relevant one locally.

## House rules, and why they exist

These are unusual enough to be worth stating, because a PR that violates them will
get comments that otherwise look arbitrary.

**One fact in one place.** Every display rule, default and threshold lives in
exactly one module — `bmv-config` for defaults, `bmv-common/src/view.rs` for
presentation. There is a test (`tests/one_place_per_rule.rs`) that fails the build
if a rule reappears somewhere else. This exists because these rules used to be
copy-pasted per shell and the copies silently diverged: the same host displayed as
"Обычный" in one shell and "Без шифра" in another, and nothing went red.

**Shells stay thin.** `apps/*` are front-ends. VPN logic belongs in `crates/`,
shared by all four shells. If you find yourself adding networking to a shell, the
change probably belongs in the core.

**We don't ship knobs that don't work.** If a config option is documented but not
read by anything, the fix is to delete it, not to fake it. Several have been
removed on exactly these grounds (`kill_switch`, `guest.dns`, `auto_reconnect`) —
a setting that promises protection it doesn't provide is worse than no setting.

**We don't write our own crypto.** Primitives come from `snow` (Noise) and
`curve25519-elligator2`. Compositions get reviewed; new primitives get rejected.

**Honest failure over hopeful spinners.** If NAT traversal can't work from
someone's network, the app says so. Please preserve that when touching error paths
— and when adding a failure mode, carry a *reason* rather than a string, so each
shell can word it for its own audience.

**Comments are in Russian.** The codebase is heavily commented and those comments
are documentation — they explain *why*, often citing the bug that motivated the
code. You are welcome to write new comments in English; nobody will ask you to
translate the existing ones, and if a Russian comment is blocking you, just ask in
the issue and it'll be translated.

## Commit messages

Look at `git log` before writing one. The convention is a descriptive sentence
saying what changed and, in the body, what was wrong before. Not `fix: bug`.

## Review

It's one maintainer, so response times vary. Partial work is welcome — a single
well-done piece beats a stalled attempt at everything. If you're planning
something large, open an issue first so we don't both build it.

---

## 🇷🇺 По-русски

**Самое полезное, что можно сделать, — поднять хост.** Модель проекта в том, что
выходная точка это чей-то компьютер, а в каталоге сейчас пусто: человек, которому
нужен доступ, видит короткий список и уходит навсегда. Кодом это не чинится.

```bash
curl -fsSL https://raw.githubusercontent.com/mister-PARADISE/bemyvpn/main/install.sh | sh
sudo bemyvpn host --tunnel --autostart
```

Переживёт выход из SSH и перезагрузку. `--hidden` — не показываться в общем списке.

**По коду.** Задачи помечены сложностью: `D-easy` (самостоятельная, знания кодовой
базы не нужны), `D-medium` (одна подсистема, прочтите её модульную доку),
`D-hard` (протокол, крипта или кишки TCP — сначала обсуждение).

Самая ценная сейчас — [локализация (#1)](https://github.com/mister-PARADISE/bemyvpn/issues/1):
интерфейс только русский, и это главный ограничитель проекта.

**Сборка** — Rust 1.85+, тесты в три прогона (координатор и форк `ipstack`
намеренно вне workspace, `--workspace` их не видит).

**Правила дома**, которые стоит знать заранее: один факт живёт в одном месте и это
проверяется тестом; оболочки тонкие, логика в ядре; ручку, которую никто не читает,
мы удаляем, а не имитируем; своей крипты не пишем; отказ обязан объяснять себя, а не
крутить спиннер. Коммиты — описательным предложением, посмотрите `git log`.
