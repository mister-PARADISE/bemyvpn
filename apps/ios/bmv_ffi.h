/* bmv_ffi.h — C-интерфейс ядра BeMyVPN для Swift (iOS/macOS).
 *
 * Все char*-результаты нужно освобождать через bmv_free_string().
 * Блокирующие вызовы (всё кроме bmv_vpn_status) — звать из фонового потока.
 */
#ifndef BMV_FFI_H
#define BMV_FFI_H

#include <stdbool.h>
#include <stdint.h>

/* Освободить строку, полученную из любой bmv_* функции. */
void  bmv_free_string(char *s);

/* ── Сигналинг (координатор) ─────────────────────────────────────────────── */
/* Живой каталог: JSON {"version":N,"hosts":[...]}. since — из прошлого ответа. */
char *bmv_list_watch(const char *coordinator, uint64_t since);
/* Найти хост по коду (в т.ч. скрытый): JSON-объект хоста или "". */
char *bmv_resolve(const char *coordinator, const char *code);
/* Новый код хоста от сервера как "CODE|SIG" ("" при ошибке). */
char *bmv_new_code(const char *coordinator);
/* Свой внешний IP через координатор ("" при ошибке). */
char *bmv_my_ip(const char *coordinator);

/* Отклик до хоста в мс, -1 = не ответил. endpoints — адреса через запятую.
   Сессию на хосте НЕ создаёт, поэтому звать можно по раскрытию карточки. */
int32_t bmv_probe_rtt(const char *coordinator, const char *host_id, const char *endpoints);
/* Быстрая проверка связи: true = сервер жив. */
bool  bmv_health(const char *coordinator);

// Круг до координатора в мс; 0 — ещё не мерили или связи нет.
// Настоящий замер (свой Ping + Pong с той же меткой), а НЕ время вызова
// bmv_health: тот читает флаг «сокет жив» и возвращается мгновенно.
uint32_t bmv_rtt_ms(const char *coordinator);

/* ── Гость: подключение ──────────────────────────────────────────────────── */
/* Фаза 1: пробитие NAT + рукопожатие БЕЗ туннеля. true — канал готов. */
bool  bmv_connect(const char *coordinator, const char *host_id,
                  const char *password, const char *protocol);
/* Фаза 2: качать пакеты через утун-fd (от Packet Tunnel).
   utun=true — снимать/добавлять 4-байтовый заголовок (iOS/macOS utun). */
bool  bmv_start_tunnel(int32_t fd, bool utun);
/* Статус: 0=выкл 1=подключаюсь 2=подключено 3=ошибка. Неблокирующая. */
int32_t bmv_vpn_status(void);
/* Почему сеанс кончился САМ: 0=не кончался (идёт или выключили сами),
   1=ХОСТ ЗАВЕРШИЛ РАЗДАЧУ, 2=связь с хостом потеряна. Неблокирующая.
   Единица — НЕ ошибка: всё сработало, хост просто выключил раздачу, и человеку
   надо предложить другой хост, а не показывать отказ. */
int32_t bmv_stop_reason(void);
/* Остановить VPN (и отменить идущее подключение). */
void  bmv_stop(void);
/* Послать хосту BYE (чистый выход) СИНХРОННО, пока туннель жив. Зовётся ДО
   stopVPNTunnel через app→extension сообщение — иначе на stopTunnel сокет уже
   мёртв и BYE не уходит. Блокирующая (до ~700мс), звать НЕ из главного потока. */
void  bmv_send_bye(void);
/* Смена сети (WiFi↔сотовая/вышка) — форсировать реконнект без падения utun.
   Зовётся из NWPathMonitor. Неблокирующая, безопасна вне сессии. */
void  bmv_nudge_reconnect(void);

/* ── Хост-режим ──────────────────────────────────────────────────────────── */
/* Стать хостом. Возвращает id, либо "!NAT" / "!SIG" / "". */
char *bmv_host_start(const char *coordinator, const char *host_id,
                     const char *token, const char *code_sig, const char *name,
                     int32_t max_guests, const char *password,
                     const char *protocol, bool is_public);
void  bmv_host_stop(void);
void  bmv_host_update(const char *name, int32_t max_guests, const char *password,
                      const char *protocol, bool is_public);

/* Логов и диагностики нет сознательно: хост не хранит записей о трафике
   гостей, поэтому и выдать их не может. */

#endif /* BMV_FFI_H */
