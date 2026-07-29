# Обращение в GitHub Support: убрать старый коммит с личными данными

Нужно один раз. После него коммит `3c4e578` перестанет открываться по прямой
ссылке — сейчас он ещё открывается, хотя из истории, blame и списка коммитов
уже исчез.

**Форма:** https://support.github.com/request
Тема: *Account or repository management* → *Something else*.

Форма на английском — текст ниже уже английский, копировать целиком.

---

**Subject:** Sensitive data removal: run GC after completed history rewrite

**Body:**

```
Hello,

I rewrote the history of my public repository to remove personal information
(my real name and machine hostname, recorded in one commit's author and
committer fields). The rewrite is already complete and force-pushed. I would
like to request the GitHub-side cleanup described in "Removing sensitive data
from a repository".

Repository:            mister-PARADISE/bemyvpn
Sensitive commit SHA:  3c4e578c94557da9ba355b65d5be044cb7c19e70
Replacement commit:    d6c0162cc5928353146b3c80564e6728f00f3f5f
Branch rewritten:      main (force-pushed)
Tags moved:            v1.6, v1.7 (force-pushed; v1.5 and older are unaffected)
Forks:                 none (fork count is 0)
Pull requests:         none (0 pull requests have ever been opened)
Git LFS:               not used in this repository
Tool used:             git filter-branch --env-filter (not git-filter-repo)

No reference in the repository points to the sensitive commit any more, but it
is still reachable directly by its SHA.

Could you please:
  - run garbage collection on the server so the unreferenced objects are removed
  - remove any cached views of that commit

Thank you.
```

---

## Как проверить, что сработало

```bash
gh api repos/mister-PARADISE/bemyvpn/commits/3c4e578
```

- **404 / «No commit found»** — готово, вопрос закрыт полностью.
- Отдаёт коммит с именем — ещё не собрано.

Пока не ответит 404, **не удаляй** резервную копию `~/bemyvpn-before-rewrite.bundle`.

## Почему это не срочно

Чтобы увидеть тот коммит, нужно заранее знать все 40 символов хеша. Он больше
не встречается ни в истории, ни в blame, ни в списке коммитов, ни в списке
контрибьюторов, ни в публичной ленте событий репозитория (а значит, и в её
сторонних архивах вроде GH Archive), ни в метаданных релизов. Форков нет,
поэтому объект ничем не удерживается и будет собран и без обращения — просто
неизвестно когда.
