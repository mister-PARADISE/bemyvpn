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

/* ── Правила показа (справочник bmv_common::view) ────────────────────────── */
/* Чистые функции: ни сети, ни блокировок — можно звать прямо с главного потока.
   Пока Swift их не звал, каждое правило жило здесь ВТОРОЙ копией на другом
   языке, и копии молча расходились. Уровни отдаются ЧИСЛОМ, а не цветом и не
   именем значка: наборы значков у оболочек разные (см. шапку view.rs). */

/* Набранный человеком адрес координатора → пригодный для работы.
   ПУСТАЯ СТРОКА — ОТКАЗ, о котором оболочка обязана сказать вслух
   («bemyvpn.net» без схемы раньше просто НЕ СОХРАНЯЛСЯ, молча). */
char *bmv_coordinator_url(const char *input);
/* Адрес координатора ДЛЯ ПОКАЗА — без схемы и без хвостового слэша. */
char *bmv_display_coordinator(const char *url);
/* Уровень защиты по имени протокола: 0 шифр · 1 маскировка · 2 без шифра ·
   3 неизвестно (номера — варианты view::Protection). */
int32_t bmv_protection(const char *protocol);
/* Часы сеанса: MM:SS, после часа H:MM:SS. */
char *bmv_session_clock(uint64_t seconds);
/* Подпись пинга («24 мс» или прочерк). Отрицательное ms — ответа не было. */
char *bmv_ping_text(int32_t ms);
/* Тревожность пинга: 0 спокойно · 1 янтарь · 2 красный · 3 приглушённо
   (номера — варианты view::Alarm). Пороги живут в справочнике. */
int32_t bmv_ping_alarm(int32_t ms);
/* Годен ли хост для подключения (живой и есть место) — на этом гасится кнопка. */
bool  bmv_host_usable(bool online, uint32_t guests, uint32_t max_guests);
/* Подпись состояния связи с координатором. online: 1 да · 0 нет · -1 не знаем. */
char *bmv_link_text(int32_t online);
/* Тревожность состояния связи (номера — варианты view::Alarm).
   Обрыв — ЯНТАРЬ: сокет чинит супервизор сам, красным тут пугать нечем. */
int32_t bmv_link_alarm(int32_t online);
/* Что написать на месте ПУСТОГО списка хостов: ждать связи или действовать. */
char *bmv_empty_directory_hint(int32_t online);
/* Состояние VPN числом: 0 выкл · 1 подключаюсь · 2 переподключение · 3 работает ·
   4 хост завершил раздачу · 5 связь потеряна (варианты view::Vpn).
   На входе — ровно то, что отдают bmv_vpn_status и bmv_stop_reason, плюс
   «сеанс уже был» (у оболочки есть отметка начала). */
int32_t bmv_vpn_kind(int32_t status, int32_t stop_reason, bool was_connected);
/* Подпись состояния VPN. Аргументы те же, что у bmv_vpn_kind — обратного
   перевода числа в состояние нарочно нет. */
char *bmv_vpn_text(int32_t status, int32_t stop_reason, bool was_connected);

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
