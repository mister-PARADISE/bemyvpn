# Обращение в GitHub Support: убрать осиротевшие объекты после переписывания истории

Нужно один раз, чтобы старый коммит перестал открываться по прямому хешу.
Ссылка на форму: **https://support.github.com/request**
(в списке тем выбрать *Account or repository management* → *Something else*).

Форма на английском, писать лучше по-английски. Текст ниже — копировать целиком.

---

**Subject:** Request garbage collection after history rewrite (remove orphaned commit)

**Body:**

```
Hello,

I rewrote the history of my public repository to remove personal information
(a real name and machine hostname that were accidentally recorded in one
commit's author/committer fields). The rewrite is complete and force-pushed:

Repository: https://github.com/mister-PARADISE/bemyvpn

The branch `main` and the tags `v1.6` and `v1.7` were updated. No reference in
the repository points to the old commit any more, and the repository has no
forks (fork count is 0).

However, the orphaned pre-rewrite commit is still reachable by its direct SHA:

    3c4e578c94557da9ba355b65d5be044cb7c19e70

Could you please run garbage collection on this repository so the unreferenced
objects are removed, and purge any cached views of that commit?

Thank you.
```

---

## Как проверить, что сработало

```bash
gh api repos/mister-PARADISE/bemyvpn/commits/3c4e578
```

- **404 / «No commit found»** — готово, вопрос закрыт полностью.
- Отдаёт коммит с именем — ещё не собрано, подождать/уточнить в тикете.

Пока не ответит 404, **не удаляй** резервную копию `~/bemyvpn-before-rewrite.bundle`.

## Почему это вообще не срочно

Чтобы увидеть тот коммит, нужно заранее знать все 40 символов хеша. Он больше
не встречается ни в истории, ни в blame, ни в списке коммитов, ни в списке
контрибьюторов, ни в публичной ленте событий репозитория (а значит, и в её
сторонних архивах вроде GH Archive), ни в метаданных релизов. Форков нет,
поэтому объект ничем не удерживается и будет собран и без обращения — просто
неизвестно когда.
