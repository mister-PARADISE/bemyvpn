# Убрать настоящее имя из GitHub: как это делается на самом деле

История уже переписана и запушена — из истории, blame, списка коммитов, списка
контрибьюторов и ленты событий имя исчезло. Остался один старый объект, который
открывается по прямому 40-символьному хешу:

```
3c4e578c94557da9ba355b65d5be044cb7c19e70
```

## Куда НЕ надо (и почему кнопок не нашлось)

Обычный путь «Support → уборка мусора после переписывания истории» **сюда не
подходит**, и это написано в документации GitHub прямым текстом:

> GitHub Support won't remove non-sensitive data, and will only assist in the
> removal of sensitive data in cases where we determine that the risk can't be
> mitigated by rotating affected credentials.

То есть тот процесс — про **утёкшие пароли, токены и ключи**, которые нельзя
просто перевыпустить. А политика удаления личных данных отдельно перечисляет,
что НЕ считается основанием:

> A person's real name by itself.

Имя и hostname под это не подпадают, поэтому в форме и нет подходящей категории.
Заявка почти наверняка вернулась бы с отказом.

## Куда надо: запрос на удаление персональных данных

Настоящее имя — это **персональные данные**, и на них у GitHub отдельный канал,
не связанный с Support:

**Писать на `privacy@github.com`** (обычным письмом, тема на английском).

Текст просьбы — в теле письма, не вложением: во вложениях они обрабатывают
дольше.

---

**Subject:** Personal data erasure request — orphaned commit in public repository

**Body:**

```
Hello,

I am the owner of the public repository mister-PARADISE/bemyvpn.

One commit in that repository was authored under my real name and my personal
machine's hostname. This is personal data that I did not intend to publish.

I have already rewritten the repository history to remove it and force-pushed
the result. No branch, tag, pull request or other reference points to the old
commit any more, and the repository has no forks. However, the original commit
object is still served by GitHub when requested directly by its SHA:

    https://github.com/mister-PARADISE/bemyvpn/commit/3c4e578c94557da9ba355b65d5be044cb7c19e70

Under my right to erasure, I request that this orphaned object and any cached
views of it be removed from GitHub's servers.

Details:
  Repository:            mister-PARADISE/bemyvpn
  Object to remove:      3c4e578c94557da9ba355b65d5be044cb7c19e70
  Replaced by commit:    d6c0162cc5928353146b3c80564e6728f00f3f5f
  History rewrite:       completed and force-pushed (branch main, tags v1.6, v1.7)
  Forks:                 none
  Pull requests:         none
  Git LFS:               not used

Thank you.
```

---

## Как проверить, что сработало

```bash
gh api repos/mister-PARADISE/bemyvpn/commits/3c4e578
```

- **404 / «No commit found»** — готово, вопрос закрыт.
- Отдаёт коммит с именем — ещё не убрано.

Пока не ответит 404, **не удаляй** резервную копию `~/bemyvpn-before-rewrite.bundle`.

## Что будет, если вообще ничего не делать

Объект ничем не удерживается: ссылок нет, форков нет. Сборщик мусора GitHub
заберёт его сам — просто неизвестно когда. Найти его до этого можно, только
заранее зная все 40 символов хеша: в истории, blame, списке коммитов, списке
контрибьюторов, ленте событий репозитория (а значит и в её сторонних архивах
вроде GH Archive) и в метаданных релизов имени нет — проверено.

То есть риск близок к нулю, а письмо — способ не зависеть от «когда-нибудь».
