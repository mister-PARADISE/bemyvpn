#!/usr/bin/env python3
"""Подставной «внешний адрес» для локального координатора — ТОЛЬКО для CI.

Зачем. Координатор публикует в каталоге НАБЛЮДАЕМЫЙ им адрес хоста и режет
приватные (`sane_addr`/`endpoints_for` в server/coordinator/src/lib.rs): анонс с
127.0.0.1 отвечает 422, и каталог на раннере остаётся пустым — то есть снимок
экрана показывал бы половину приложения.

Обход ровно тот же, каким пользуется собственный тестовый харнесс координатора
(lib.rs, mod ws_life): запрос с петли считается пришедшим от своего обратного
прокси, поэтому заголовку `X-Forwarded-For` он верит без всяких настроек. Этот
скрипт и есть такой «прокси» на 40 строк: слушает порт, вставляет XFF в первый
HTTP-запрос (WebSocket-рукопожатие — обычный HTTP), дальше просто переливает
байты в обе стороны.

Запуск:  python3 tools/ci-xff-relay.py 3330 3331:8.8.8.8 3332:77.88.55.88
         (первый аргумент — порт координатора, дальше пары «порт:адрес»)
"""

import socket
import sys
import threading


def pipe(src, dst):
    """Переливать байты, пока не кончатся; на любой обрыв — закрыть обе стороны."""
    try:
        while True:
            chunk = src.recv(65536)
            if not chunk:
                break
            dst.sendall(chunk)
    except OSError:
        pass
    finally:
        for s in (src, dst):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def handle(cli, upstream_port, xff):
    try:
        up = socket.create_connection(("127.0.0.1", upstream_port))
    except OSError:
        cli.close()
        return
    # Дочитываем ровно заголовки первого запроса (тела у WS-рукопожатия нет).
    head = b""
    while b"\r\n\r\n" not in head:
        chunk = cli.recv(65536)
        if not chunk:
            cli.close()
            up.close()
            return
        head += chunk
    cut = head.index(b"\r\n\r\n")
    head = head[:cut] + b"\r\nX-Forwarded-For: " + xff.encode() + head[cut:]
    up.sendall(head)
    threading.Thread(target=pipe, args=(cli, up), daemon=True).start()
    pipe(up, cli)


def listen(port, upstream_port, xff):
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(16)
    print(f"relay 127.0.0.1:{port} -> 127.0.0.1:{upstream_port} as {xff}", flush=True)
    while True:
        cli, _ = srv.accept()
        threading.Thread(target=handle, args=(cli, upstream_port, xff), daemon=True).start()


if __name__ == "__main__":
    coord = int(sys.argv[1])
    for arg in sys.argv[2:]:
        port, ip = arg.split(":")
        threading.Thread(target=listen, args=(int(port), coord, ip), daemon=True).start()
    threading.Event().wait()
