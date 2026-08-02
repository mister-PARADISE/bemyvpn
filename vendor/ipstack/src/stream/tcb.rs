use super::seqnum::SeqNum;
use etherparse::TcpHeader;
use std::{collections::BTreeMap, time::Duration};

pub(super) const MAX_UNACK: u32 = 1024 * 16; // 16KB
pub(super) const READ_BUFFER_SIZE: usize = 1024 * 16; // 16KB
pub(super) const MAX_COUNT_FOR_DUP_ACK: usize = 3; // Maximum number of duplicate ACKs before retransmission

/// Retransmission timeout — НАЧАЛЬНОЕ значение (RFC 6298 §2.1) до первого замера
/// RTT. Дальше RTO АДАПТИВНЫЙ: srtt + 4·rttvar, зажат в [MIN_RTO, MAX_RTO].
pub(super) const RTO: std::time::Duration = std::time::Duration::from_secs(1);
/// BeMyVPN fork: нижняя граница RTO. Фиксированная 1с превращала КАЖДУЮ потерю
/// на быстром пути (RTT ~20мс) в секундный столл; 200мс — как у Linux tcp_rto_min.
pub(super) const MIN_RTO: std::time::Duration = std::time::Duration::from_millis(200);
/// BeMyVPN fork: верхняя граница RTO (RFC 6298 рекомендует ≥60с).
pub(super) const MAX_RTO: std::time::Duration = std::time::Duration::from_secs(60);

/// Maximum count of retransmissions before dropping the packet
pub(super) const MAX_RETRANSMIT_COUNT: usize = 3;

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum TcpState {
    // Init, /* Since we always act as a server, it starts from `Listen`, so we don't use states Init & SynSent. */
    // SynSent,
    Listen,
    SynReceived,
    Established,
    FinWait1, // act as a client, actively send a farewell packet to the other side, followed with FinWait2, TimeWait, Closed
    FinWait2,
    TimeWait,
    CloseWait, // act as a server, followed with LastAck, Closed
    LastAck,
    Closed,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub(super) enum PacketType {
    WindowUpdate,
    Invalid,
    RetransmissionRequest,
    NewPacket,
    Ack,
    KeepAlive,
}

/// TCP Control Block
/// - `inflight_packets` is prerepresented bytes stream from upstream application,
///   which have been sent to the lower device but not yet acknowledged.
/// - `unordered_packets` is the bytes stream received from the lower device,
///   which can be acknowledged and extracted by `consume_unordered_packets` method
///   then can be read by upstream application via `Tcp::poll_read` method.
#[derive(Debug, Clone)]
pub(crate) struct Tcb {
    seq: SeqNum,
    ack: SeqNum,
    mtu: u16,
    last_received_ack: SeqNum,
    send_window: u16,
    /// BeMyVPN fork: TCP window scaling (RFC 7323). `snd_wnd_shift` — сдвиг,
    /// объявленный пиром в SYN (его window-поля умножаются на 2^shift);
    /// `rcv_wnd_shift` — наш сдвиг из SYN-ACK (наши window-поля делятся).
    /// Оба 0, если пир не прислал wscale (точное поведение оригинала).
    snd_wnd_shift: u8,
    rcv_wnd_shift: u8,
    wscale_negotiated: bool,
    /// BeMyVPN fork: примитивный контроль перегрузки (slow start + AIMD).
    /// Оригинал слал СРАЗУ всё окно (с wscale — мегабайты одним взрывом) →
    /// переполнение буферов пути → массовая потеря → коллапс. cwnd начинается
    /// с 16×MSS, удваивается за RTT (slow start) до ssthresh, дальше +MSS за
    /// окно (congestion avoidance); при потере — деление пополам.
    cwnd: u32,
    ssthresh: u32,
    /// Верхняя граница уже учтённых в росте cwnd подтверждённых байт.
    cwnd_acked: SeqNum,
    state: TcpState,
    inflight_packets: BTreeMap<SeqNum, InflightPacket>,
    unordered_packets: BTreeMap<SeqNum, Vec<u8>>,
    /// BeMyVPN fork: суммарный объём `unordered_packets`, поддерживается при
    /// каждой вставке/изъятии. Раньше сумму считали проходом по всей карте, а
    /// зовут её на КАЖДОМ пакете (приёмное окно) — теперь это O(1).
    unordered_bytes: usize,
    /// BeMyVPN fork: байты, уже отданные наверх в `data_tx` (unbounded!), но
    /// ещё не прочитанные приложением. Без их учёта приёмное окно не отражало
    /// реальную загрузку приёмника, и притормозить быстрого пира было нечем.
    /// Живёт под тем же `Mutex<Tcb>`, что и всё остальное — атомик не нужен.
    upstream_queued: usize,
    duplicate_ack_count: usize,
    duplicate_ack_count_helper: SeqNum,
    max_unacked_bytes: u32,
    read_buffer_size: usize,
    max_count_for_dup_ack: usize,
    rto: std::time::Duration,
    max_retransmit_count: usize,
    /// BeMyVPN fork: адаптивный RTO (RFC 6298). `srtt`=None до первого замера RTT;
    /// дальше сглаженный RTT и его вариация задают `rto`.
    srtt: Option<std::time::Duration>,
    rttvar: std::time::Duration,
    /// BeMyVPN fork: fast-recovery guard. Один сигнал потери на ОКНО, а не на
    /// каждый dup-ACK: без него первый же loss на жирном канале давал целое окно
    /// dup-ACK'ов, каждый ХАЛВИЛ cwnd → окно схлопывалось в пол (4·MSS).
    in_recovery: bool,
    /// seq на момент входа в recovery — по достижении его кумулятивным ACK выходим.
    recover: SeqNum,
}

impl Tcb {
    pub(super) fn new(
        ack: SeqNum,
        mtu: u16,
        max_unacked_bytes: u32,
        read_buffer_size: usize,
        max_count_for_dup_ack: usize,
        rto: std::time::Duration,
        max_retransmit_count: usize,
    ) -> Tcb {
        #[cfg(debug_assertions)]
        let seq = 100;
        #[cfg(not(debug_assertions))]
        let seq = rand::Rng::random::<u32>(&mut rand::rng());
        Tcb {
            seq: seq.into(),
            ack,
            mtu,
            last_received_ack: seq.into(),
            send_window: u16::MAX,
            snd_wnd_shift: 0,
            rcv_wnd_shift: 0,
            wscale_negotiated: false,
            cwnd: (mtu as u32).saturating_mul(16), // стартовое окно ~16 пакетов
            ssthresh: u32::MAX,
            cwnd_acked: seq.into(),
            state: TcpState::Listen,
            inflight_packets: BTreeMap::new(),
            unordered_packets: BTreeMap::new(),
            unordered_bytes: 0,
            upstream_queued: 0,
            duplicate_ack_count: 0,
            duplicate_ack_count_helper: seq.into(),
            max_unacked_bytes,
            read_buffer_size,
            max_count_for_dup_ack,
            rto,
            max_retransmit_count,
            srtt: None,
            rttvar: std::time::Duration::ZERO,
            in_recovery: false,
            recover: seq.into(),
        }
    }

    pub fn calculate_payload_max_len(&self, ip_header_size: usize, tcp_header_size: usize) -> usize {
        let send_window = self.get_send_window() as usize;
        let mtu = self.get_mtu() as usize;
        std::cmp::min(send_window, mtu.saturating_sub(ip_header_size + tcp_header_size))
    }

    pub fn update_duplicate_ack_count(&mut self, rcvd_ack: SeqNum) {
        // If the received rcvd_ack is the same as duplicate_ack_count_helper and not all data has been acknowledged (rcvd_ack < self.seq), increment the count.
        if rcvd_ack == self.duplicate_ack_count_helper && rcvd_ack < self.seq {
            self.duplicate_ack_count = self.duplicate_ack_count.saturating_add(1);
        } else {
            self.duplicate_ack_count_helper = rcvd_ack;
            self.duplicate_ack_count = 0; // reset duplicate ACK count
        }
    }

    pub fn is_duplicate_ack_count_exceeded(&self) -> bool {
        self.duplicate_ack_count >= self.max_count_for_dup_ack
    }

    pub(super) fn add_unordered_packet(&mut self, seq: SeqNum, buf: Vec<u8>) {
        if seq < self.ack {
            #[rustfmt::skip]
            log::warn!("{:?}: Received packet seq {seq} < self ack {}, len = {}", self.state, self.ack, buf.len());
            return;
        }
        // BeMyVPN fork: ВЕРХНИЙ ПРЕДЕЛ буфера пересборки. Без него гость, шлющий
        // сегменты с пропуском (seq = ack + 1360·k), навсегда закреплял память
        // хоста: дыра на self.ack не закроется никогда, а класть мы продолжали.
        //
        // Порог = read_buffer_size, то есть МАКСИМУМ приёмного окна, который мы
        // когда-либо объявляли пиру. Меньше брать нельзя: при потере ПЕРВОГО
        // сегмента окна всё остальное окно (законные данные, крупный реордеринг
        // мобильного пути) приезжает вне порядка и обязано поместиться — иначе
        // мы дропаем то, что сами же разрешили прислать, и платим лишними
        // ретрансмитами. Больше — незачем: пир по объявленному окну столько и
        // не пришлёт. Потолок памяти на соединение = 2 × read_buffer_size
        // (буфер пересборки + очередь наверх), и это ОГРАНИЧЕНО, в отличие от
        // прежнего «сколько пришлёт гость».
        //
        // Сегмент РОВНО на self.ack пропускаем всегда, даже сверх предела: он
        // закрывает дыру и тут же уходит наверх. Дропнуть его = вечный тупик
        // (пир ретрансмитит — мы дропаем — буфер не пустеет).
        if seq != self.ack && self.unordered_bytes.saturating_add(buf.len()) > self.read_buffer_size {
            #[rustfmt::skip]
            log::debug!("{:?}: reassembly buffer full ({} B), dropping out-of-order seq {seq}, len = {}", self.state, self.unordered_bytes, buf.len());
            return; // TCP восстановит: пир ретрансмитит после нашего dup-ACK
        }
        self.unordered_bytes += buf.len();
        if let Some(old) = self.unordered_packets.insert(seq, buf) {
            self.unordered_bytes -= old.len(); // тот же seq пришёл повторно
        }
    }
    pub(super) fn get_available_read_buffer_size(&self) -> usize {
        self.read_buffer_size
            .saturating_sub(self.unordered_bytes)
            .saturating_sub(self.upstream_queued)
    }
    #[inline]
    pub(crate) fn get_unordered_packets_total_len(&self) -> usize {
        self.unordered_bytes
    }
    /// BeMyVPN fork: данные ушли в очередь наверх — занимают приёмный буфер,
    /// пока приложение их не прочитает (см. `release_upstream_queued`).
    pub(super) fn reserve_upstream_queued(&mut self, len: usize) {
        self.upstream_queued = self.upstream_queued.saturating_add(len);
    }
    /// BeMyVPN fork: приложение прочитало — место в приёмном буфере вернулось.
    pub(super) fn release_upstream_queued(&mut self, len: usize) {
        self.upstream_queued = self.upstream_queued.saturating_sub(len);
    }

    pub(super) fn consume_unordered_packets(&mut self, max_bytes: usize) -> Option<Vec<u8>> {
        let mut data = Vec::new();
        let mut remaining_bytes = max_bytes;

        while remaining_bytes > 0 {
            if let Some(seq) = self.unordered_packets.keys().next().copied() {
                if seq != self.ack {
                    break; // sequence number is not continuous, stop extracting
                }

                // remove and get the first packet
                let mut payload = self.unordered_packets.remove(&seq).unwrap();
                let payload_len = payload.len();

                if payload_len <= remaining_bytes {
                    // current packet can be fully extracted
                    data.extend(payload);
                    self.ack += payload_len as u32;
                    self.unordered_bytes -= payload_len;
                    remaining_bytes -= payload_len;
                } else {
                    // current packet can only be partially extracted
                    let remaining_payload = payload.split_off(remaining_bytes);
                    data.extend_from_slice(&payload);
                    self.ack += remaining_bytes as u32;
                    self.unordered_bytes -= remaining_bytes;
                    self.unordered_packets.insert(self.ack, remaining_payload);
                    break;
                }
            } else {
                break; // no more packets to extract
            }
        }

        if data.is_empty() { None } else { Some(data) }
    }

    pub(super) fn increase_seq(&mut self) {
        self.seq += 1;
    }
    pub(super) fn get_seq(&self) -> SeqNum {
        self.seq
    }
    pub(super) fn increase_ack(&mut self) {
        self.ack += 1;
    }
    pub(super) fn get_ack(&self) -> SeqNum {
        self.ack
    }
    pub(super) fn get_mtu(&self) -> u16 {
        self.mtu
    }
    pub(super) fn get_last_received_ack(&self) -> SeqNum {
        self.last_received_ack
    }
    pub(super) fn change_state(&mut self, state: TcpState) {
        self.state = state;
    }
    pub(super) fn get_state(&self) -> TcpState {
        self.state
    }
    pub(super) fn update_send_window(&mut self, window: u16) {
        self.send_window = window;
    }
    /// BeMyVPN fork: включить window scaling после разбора SYN пира.
    /// `snd` — сдвиг пира (его окна ×2^snd), `rcv` — наш (наши окна ÷2^rcv).
    pub(super) fn set_window_shifts(&mut self, snd: u8, rcv: u8) {
        self.snd_wnd_shift = snd.min(14); // RFC 7323: сдвиг не больше 14
        self.rcv_wnd_shift = rcv.min(14);
        self.wscale_negotiated = true;
    }
    pub(super) fn get_rcv_wnd_shift(&self) -> u8 {
        self.rcv_wnd_shift
    }
    /// wscale согласован? (пир прислал опцию в SYN → мы обязаны ответить своей
    /// в SYN-ACK, даже если наш сдвиг 0 — иначе RFC отключает масштаб целиком).
    pub(super) fn is_wscale_negotiated(&self) -> bool {
        self.wscale_negotiated
    }
    /// Эффективное окно пира В БАЙТАХ (raw-поле × 2^shift). До рукопожатия
    /// shift=0 → в SYN поле не масштабируется, как велит RFC.
    pub(super) fn get_send_window(&self) -> u32 {
        (self.send_window as u32) << self.snd_wnd_shift
    }
    /// Значение window-ПОЛЯ для наших исходящих пакетов: свободный приёмный
    /// буфер, поделённый на наш масштаб. При включённом wscale буфер >64КБ
    /// теперь виден пиру целиком.
    ///
    /// BeMyVPN fork: раньше здесь стоял `avail.max(self.mtu)` — окно НИКОГДА не
    /// закрывалось, то есть обратной связи по приёмнику не существовало вовсе:
    /// быстрый гость лил во весь канал, а очередь наверх (unbounded) росла.
    /// Теперь окно честно доходит до нуля. По RFC 1122 §4.2.3.3 (silly window
    /// syndrome на приёмнике) объявляем либо ноль, либо хотя бы сегмент —
    /// дробных окон пир не увидит. Порог берём min(MTU, буфер/2), иначе при
    /// read_buffer_size < MTU окно залипло бы в нуле навсегда.
    pub(super) fn get_recv_window(&self) -> u16 {
        let avail = self.get_available_read_buffer_size();
        let threshold = (self.mtu as usize).min(self.read_buffer_size / 2).max(1);
        if avail < threshold {
            return 0;
        }
        // `.max(1)`: место есть, значит объявлять ноль НЕЛЬЗЯ — пир ушёл бы в
        // persist навсегда (мы бы отвечали нулём на каждый зонд). Округление в
        // ноль возможно лишь при абсурдно большом сдвиге (буфер сотни МБ).
        (avail >> self.rcv_wnd_shift).try_into().unwrap_or(u16::MAX).max(1)
    }
    // #[inline(always)]
    // pub(super) fn buffer_size(&self, payload_len: u16) -> u16 {
    //     match MAX_UNACK - self.inflight_packets.len() as u32 {
    //         // b if b.saturating_sub(payload_len as u32 + 64) != 0 => payload_len,
    //         // b if b < 128 && b >= 4 => (b / 2) as u16,
    //         // b if b < 4 => b as u16,
    //         // b => (b - 64) as u16,
    //         b if b >= payload_len as u32 * 2 && b > 0 => payload_len,
    //         b if b < 4 => b as u16,
    //         b => (b / 2) as u16,
    //     }
    // }

    pub(super) fn check_pkt_type(&self, tcp_header: &TcpHeader, payload: &[u8]) -> PacketType {
        let rcvd_ack = SeqNum(tcp_header.acknowledgment_number);
        let rcvd_seq = SeqNum(tcp_header.sequence_number);
        let rcvd_window = tcp_header.window_size;
        let len = payload.len();
        let res = if rcvd_ack > self.seq {
            PacketType::Invalid
        } else {
            match rcvd_ack.cmp(&self.get_last_received_ack()) {
                std::cmp::Ordering::Less => PacketType::Invalid,
                std::cmp::Ordering::Equal => {
                    if self.ack - 1 == rcvd_seq && payload.len() <= 1 {
                        PacketType::KeepAlive
                    } else if !payload.is_empty() {
                        PacketType::NewPacket
                    } else if self.seq != rcvd_ack && self.is_duplicate_ack_count_exceeded() {
                        // BeMyVPN fork: dup-ACK определяется кумулятивным ACK'ом (тот
                        // же ack, без данных, есть неподтверждённое), а НЕ равенством
                        // окна. Прежнее условие `get_send_window_raw()==rcvd_window`
                        // ГЛУШИЛО fast-retransmit на реальном трафике, где приёмное
                        // окно пира гуляет почти на каждом ACK → любая потеря ждала
                        // полный RTO. Само окно обновляется отдельно (update_send_window).
                        PacketType::RetransmissionRequest
                    } else {
                        PacketType::WindowUpdate
                    }
                }
                std::cmp::Ordering::Greater => {
                    if payload.is_empty() {
                        PacketType::Ack
                    } else {
                        PacketType::NewPacket
                    }
                }
            }
        };
        #[rustfmt::skip]
        log::trace!("received {{ ack = {:08X?}, seq = {:08X?}, window = {rcvd_window} }}, self {{ ack = {:08X?}, seq = {:08X?}, send_window = {} }}, len = {len}, {res:?}", rcvd_ack.0, rcvd_seq.0, self.ack.0, self.seq.0, self.get_send_window());
        res
    }

    pub(super) fn add_inflight_packet(&mut self, buf: Vec<u8>) -> std::io::Result<()> {
        if buf.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Empty payload"));
        }
        let buf_len = buf.len() as u32;
        self.inflight_packets.insert(self.seq, InflightPacket::new(self.seq, buf, self.rto));
        self.seq += buf_len;
        Ok(())
    }

    pub(super) fn update_last_received_ack(&mut self, ack: SeqNum) {
        self.last_received_ack = ack;
    }

    /// BeMyVPN fork: рост cwnd на свежеподтверждённые байты.
    fn grow_cwnd(&mut self, ack: SeqNum) {
        if ack <= self.cwnd_acked {
            return;
        }
        let newly = ack.distance(self.cwnd_acked);
        self.cwnd_acked = ack;
        let mss = self.mtu as u32;
        if self.cwnd < self.ssthresh {
            // slow start: +байт за байт = удвоение за RTT
            self.cwnd = self.cwnd.saturating_add(newly);
        } else {
            // congestion avoidance: ~+MSS за окно
            self.cwnd = self.cwnd.saturating_add((mss.saturating_mul(newly) / self.cwnd.max(1)).max(1));
        }
        self.cwnd = self.cwnd.min(self.max_unacked_bytes.max(mss));
    }

    /// BeMyVPN fork: реакция на потерю (таймаут ретрансмита или dup-ACK) —
    /// мультипликативное снижение, как в классическом TCP.
    pub(crate) fn on_congestion(&mut self) {
        let mss = self.mtu as u32;
        self.ssthresh = (self.cwnd / 2).max(mss.saturating_mul(4));
        self.cwnd = self.ssthresh;
    }

    /// BeMyVPN fork: замер RTT по свежеподтверждённому НЕретрансмитнутому сегменту
    /// (алгоритм Карна — ретрансмиты пропускаем, их RTT неоднозначен) и пересчёт
    /// RTO по RFC 6298. Зовётся ДО удаления подтверждённых пакетов из очереди.
    fn sample_rtt(&mut self, ack: SeqNum) {
        let now = std::time::Instant::now();
        // самый свежий (высший seq) полностью подтверждённый неретрансмитнутый сегмент
        let sample = self
            .inflight_packets
            .iter()
            .filter(|(_, p)| p.retransmit_count == 0 && ack >= p.seq + p.payload.len() as u32)
            .max_by_key(|(s, _)| **s)
            .map(|(_, p)| now.saturating_duration_since(p.send_time));
        let Some(rtt) = sample else { return };
        match self.srtt {
            None => {
                self.srtt = Some(rtt);
                self.rttvar = rtt / 2;
            }
            Some(srtt) => {
                let delta = if srtt > rtt { srtt - rtt } else { rtt - srtt };
                self.rttvar = self.rttvar.mul_f64(0.75) + delta.mul_f64(0.25);
                self.srtt = Some(srtt.mul_f64(0.875) + rtt.mul_f64(0.125));
            }
        }
        let srtt = self.srtt.unwrap_or(rtt);
        self.rto = (srtt + 4 * self.rttvar).clamp(MIN_RTO, MAX_RTO);
    }

    /// BeMyVPN fork: войти в fast-recovery ОДИН раз на событие потери.
    /// `true` → первый dup-ACK-триггер (снизить cwnd и ретрансмитить);
    /// `false` → уже восстанавливаемся (cwnd повторно НЕ режем).
    pub(crate) fn enter_fast_recovery(&mut self) -> bool {
        if self.in_recovery {
            return false;
        }
        self.in_recovery = true;
        self.recover = self.seq; // все данные, отправленные ДО потери
        self.on_congestion();
        true
    }

    pub(crate) fn update_inflight_packet_queue(&mut self, ack: SeqNum) {
        self.grow_cwnd(ack);
        self.sample_rtt(ack); // BeMyVPN fork: адаптивный RTO — до удаления пакетов
        // BeMyVPN fork: кумулятивный ACK ушёл за точку потери → выходим из recovery
        // (следующая потеря снова снизит cwnd ровно один раз).
        if self.in_recovery && ack >= self.recover {
            self.in_recovery = false;
        }
        match self.inflight_packets.first_key_value() {
            None => return,
            Some((&seq, _)) if ack < seq => return,
            _ => {}
        }
        if let Some(seq) = self
            .inflight_packets
            .iter()
            .find(|(_, p)| p.contains_seq_num(ack - 1))
            .map(|(&s, _)| s)
        {
            let mut inflight_packet = self.inflight_packets.remove(&seq).unwrap();
            let distance = ack.distance(inflight_packet.seq) as usize;
            if distance < inflight_packet.payload.len() {
                inflight_packet.payload.drain(0..distance);
                inflight_packet.seq = ack;
                self.inflight_packets.insert(ack, inflight_packet);
            }
        }
        self.inflight_packets.retain(|_, p| ack < p.seq + p.payload.len() as u32);
    }

    pub(crate) fn find_inflight_packet(&self, seq: SeqNum) -> Option<&InflightPacket> {
        self.inflight_packets.get(&seq)
    }

    #[must_use]
    pub(crate) fn collect_timed_out_inflight_packets(&mut self) -> Vec<InflightPacket> {
        let mut retransmit_list = Vec::new();

        self.inflight_packets.retain(|_, packet| {
            if packet.retransmit_count >= self.max_retransmit_count {
                log::warn!("Packet with seq {:?} reached max retransmit count, dropping packet", packet.seq);
                return false; // remove this packet
            }
            if packet.is_timed_out() {
                packet.retransmit_count += 1;
                packet.retransmit_timeout *= 2; // increase timeout exponentially
                packet.send_time = std::time::Instant::now();
                retransmit_list.push(packet.clone());
            }
            true // keep the packet in the inflight_packets
        });
        if !retransmit_list.is_empty() {
            self.on_congestion(); // BeMyVPN fork: таймаут = сигнал перегрузки
        }
        retransmit_list
    }

    pub(crate) fn get_inflight_packets_total_len(&self) -> usize {
        self.inflight_packets.values().map(|p| p.payload.len()).sum()
    }

    #[allow(dead_code)]
    pub(crate) fn get_all_inflight_packets(&self) -> Vec<&InflightPacket> {
        self.inflight_packets.values().collect::<Vec<_>>()
    }

    pub fn is_send_buffer_full(&self) -> bool {
        // BeMyVPN fork: настоящий min(cwnd, rwnd, конфиг) — cwnd не даёт
        // высыпать всё (теперь большое, спасибо wscale) окно одним взрывом.
        let limit = self.max_unacked_bytes.min(self.get_send_window()).min(self.cwnd);
        self.seq.distance(self.get_last_received_ack()) >= limit
    }
}

#[derive(Debug, Clone)]
pub struct InflightPacket {
    pub seq: SeqNum,
    pub payload: Vec<u8>,
    pub send_time: std::time::Instant,
    pub retransmit_count: usize,
    pub retransmit_timeout: std::time::Duration, // current retransmission timeout
}

impl InflightPacket {
    fn new(seq: SeqNum, payload: Vec<u8>, rto: Duration) -> Self {
        Self {
            seq,
            payload,
            send_time: std::time::Instant::now(),
            retransmit_count: 0,
            retransmit_timeout: rto,
        }
    }
    pub(crate) fn contains_seq_num(&self, seq: SeqNum) -> bool {
        self.seq <= seq && seq < self.seq + self.payload.len() as u32
    }
    pub(crate) fn is_timed_out(&self) -> bool {
        self.send_time.elapsed() >= self.retransmit_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_flight_packet() {
        let p = InflightPacket::new((u32::MAX - 1).into(), vec![10, 20, 30, 40, 50], RTO);

        assert!(p.contains_seq_num((u32::MAX - 1).into()));
        assert!(p.contains_seq_num(u32::MAX.into()));
        assert!(p.contains_seq_num(0.into()));
        assert!(p.contains_seq_num(1.into()));
        assert!(p.contains_seq_num(2.into()));

        assert!(!p.contains_seq_num(3.into()));
    }

    #[test]
    fn test_get_unordered_packets_with_max_bytes() {
        let mut tcb = Tcb::new(
            SeqNum(1000),
            1500,
            MAX_UNACK,
            READ_BUFFER_SIZE,
            MAX_COUNT_FOR_DUP_ACK,
            RTO,
            MAX_RETRANSMIT_COUNT,
        );

        // insert 3 consecutive packets
        tcb.add_unordered_packet(SeqNum(1000), vec![1; 500]); // seq=1000, len=500
        tcb.add_unordered_packet(SeqNum(1500), vec![2; 500]); // seq=1500, len=500
        tcb.add_unordered_packet(SeqNum(2000), vec![3; 500]); // seq=2000, len=500

        // test 1: extract up to 700 bytes
        let data = tcb.consume_unordered_packets(700).unwrap();
        assert_eq!(data.len(), 700); // extract 500 + 200
        assert_eq!(data[..500], vec![1; 500]); // the first packet
        assert_eq!(data[500..700], vec![2; 200]); // the first 200 bytes of the second packet
        assert_eq!(tcb.ack, SeqNum(1700)); // ack increased by 700
        assert_eq!(tcb.unordered_packets.len(), 2); // remaining two packets
        assert_eq!(tcb.unordered_packets.get(&SeqNum(1700)).unwrap().len(), 300); // the second packet remaining 300 bytes
        assert_eq!(tcb.unordered_packets.get(&SeqNum(2000)).unwrap().len(), 500); // the third packet unchanged

        // test 2: extract up to 800 bytes
        let data = tcb.consume_unordered_packets(800).unwrap();
        assert_eq!(data.len(), 800); // extract 300 bytes of the second packet and the third packet
        assert_eq!(data[..300], vec![2; 300]); // the remaining 300 bytes of the second packet
        assert_eq!(data[300..800], vec![3; 500]); // the third packet
        assert_eq!(tcb.ack, SeqNum(2500)); // ack increased by 800
        assert_eq!(tcb.unordered_packets.len(), 0); // no remaining packets

        // test 3: no data to extract
        let data = tcb.consume_unordered_packets(1000);
        assert!(data.is_none());
    }

    #[test]
    fn test_update_inflight_packet_queue() {
        let mut tcb = Tcb::new(
            SeqNum(1000),
            1500,
            MAX_UNACK,
            READ_BUFFER_SIZE,
            MAX_COUNT_FOR_DUP_ACK,
            RTO,
            MAX_RETRANSMIT_COUNT,
        );
        tcb.seq = SeqNum(100); // setting the initial seq

        // insert 3 consecutive packets
        tcb.add_inflight_packet(vec![1; 500]).unwrap(); // seq=100, len=500
        tcb.add_inflight_packet(vec![2; 500]).unwrap(); // seq=600, len=500
        tcb.add_inflight_packet(vec![3; 500]).unwrap(); // seq=1100, len=500

        // test 1: confirm partial packets (ack=800)
        tcb.update_inflight_packet_queue(SeqNum(800));
        assert_eq!(tcb.inflight_packets.len(), 2); // remaining two packets
        let first_packet = tcb.inflight_packets.first_key_value().unwrap().1;
        assert_eq!(first_packet.seq, SeqNum(800)); // the remaining part of the first packet
        assert_eq!(first_packet.payload.len(), 300); // remaining 300 bytes in the first packet
        let second_packet = tcb.inflight_packets.last_key_value().unwrap().1;
        assert_eq!(second_packet.seq, SeqNum(1100)); // no change in the second packet

        // test 2: confirm all packets (ack=2000)
        tcb.update_inflight_packet_queue(SeqNum(2000));
        assert_eq!(tcb.inflight_packets.len(), 0); // all packets are acknowledged
    }

    #[test]
    fn test_update_inflight_packet_queue_cumulative_ack() {
        let mut tcb = Tcb::new(
            SeqNum(1000),
            1500,
            MAX_UNACK,
            READ_BUFFER_SIZE,
            MAX_COUNT_FOR_DUP_ACK,
            RTO,
            MAX_RETRANSMIT_COUNT,
        );
        tcb.seq = SeqNum(1000);

        // Insert 3 consecutive packets
        tcb.add_inflight_packet(vec![1; 500]).unwrap(); // seq=1000, len=500
        tcb.add_inflight_packet(vec![2; 500]).unwrap(); // seq=1500, len=500
        tcb.add_inflight_packet(vec![3; 500]).unwrap(); // seq=2000, len=500

        // Emulate cumulative ACK: ack=2500
        tcb.update_inflight_packet_queue(SeqNum(2500));
        assert_eq!(tcb.inflight_packets.len(), 0); // all packets should be removed
    }

    #[test]
    fn adaptive_rto_shrinks_from_measured_rtt() {
        // BeMyVPN fork: RTO больше не фиксирован 1с — он подстраивается под RTT.
        let mut tcb = Tcb::new(SeqNum(1000), 1400, MAX_UNACK, READ_BUFFER_SIZE, MAX_COUNT_FOR_DUP_ACK, RTO, MAX_RETRANSMIT_COUNT);
        tcb.seq = SeqNum(1000);
        assert_eq!(tcb.rto, RTO); // старт — начальная 1с
        tcb.add_inflight_packet(vec![1; 500]).unwrap(); // seq 1000..1500
        // Мгновенный полный ACK → измеренный RTT ~0 → RTO зажимается в нижний предел.
        tcb.update_inflight_packet_queue(SeqNum(1500));
        assert!(tcb.srtt.is_some(), "должен появиться замер srtt");
        assert_eq!(tcb.rto, MIN_RTO, "крошечный RTT → RTO = нижняя граница");
        assert!(tcb.rto < RTO, "адаптивный RTO меньше фиксированной 1с");
    }

    #[test]
    fn fast_recovery_halves_cwnd_once_per_loss() {
        // BeMyVPN fork: cwnd снижается ОДИН раз на потерю, а не на каждый dup-ACK.
        let mut tcb = Tcb::new(SeqNum(1000), 1400, MAX_UNACK, READ_BUFFER_SIZE, MAX_COUNT_FOR_DUP_ACK, RTO, MAX_RETRANSMIT_COUNT);
        tcb.seq = SeqNum(5000);
        let cwnd0 = tcb.cwnd;
        assert!(tcb.enter_fast_recovery(), "первый dup-ACK-триггер → входим");
        let cwnd1 = tcb.cwnd;
        assert!(cwnd1 < cwnd0, "cwnd снижен один раз");
        // Ещё десяток dup-ACK'ов того же окна — cwnd НЕ должен падать дальше.
        for _ in 0..10 {
            assert!(!tcb.enter_fast_recovery(), "во время recovery повторно НЕ входим");
        }
        assert_eq!(tcb.cwnd, cwnd1, "cwnd не рушится в пол на каждом dup-ACK");
        // Кумулятивный ACK ушёл за точку потери → выходим, следующая потеря снова снизит.
        let r = tcb.recover;
        tcb.update_inflight_packet_queue(r + 1);
        assert!(!tcb.in_recovery, "recovery завершилась");
        assert!(tcb.enter_fast_recovery(), "новая потеря → снова можно снизить");
    }

    /// Компактный Tcb для тестов буфера пересборки: MTU 1400, буфер `buf`.
    fn tcb_with_read_buffer(buf: usize) -> Tcb {
        let mut tcb = Tcb::new(SeqNum(1000), 1400, MAX_UNACK, buf, MAX_COUNT_FOR_DUP_ACK, RTO, MAX_RETRANSMIT_COUNT);
        tcb.change_state(TcpState::Established);
        tcb
    }

    #[test]
    fn unordered_buffer_is_bounded_by_read_buffer_size() {
        // BeMyVPN fork (OOM): гость шлёт сегменты С ПРОПУСКОМ (дыра на self.ack
        // никогда не закрывается) — раньше они копились БЕЗ ВЕРХНЕГО ПРЕДЕЛА и
        // навсегда закрепляли память хоста. Один гость = сколько угодно памяти.
        let buf = 16 * 1024;
        let mut tcb = tcb_with_read_buffer(buf);
        // Дыра: первый байт (seq 1000) не приходит НИКОГДА.
        for k in 0..1000u32 {
            tcb.add_unordered_packet(SeqNum(1001 + k * 1360), vec![0u8; 1360]);
        }
        let total = tcb.get_unordered_packets_total_len();
        assert!(total <= buf, "буфер пересборки разросся до {total} при пределе {buf}");
    }

    #[test]
    fn in_order_segment_is_accepted_even_when_buffer_is_full() {
        // Предел НЕ должен ронять сегмент, закрывающий дыру: иначе пир будет
        // вечно его ретрансмитить, а мы вечно дропать — соединение мертво.
        let buf = 4096;
        let mut tcb = tcb_with_read_buffer(buf);
        for k in 0..10u32 {
            tcb.add_unordered_packet(SeqNum(1001 + k * 1360), vec![0u8; 1360]);
        }
        assert!(tcb.get_unordered_packets_total_len() >= buf - 1360, "буфер должен быть заполнен");
        tcb.add_unordered_packet(SeqNum(1000), vec![7u8; 1]); // ровно на self.ack
        let data = tcb.consume_unordered_packets(64 * 1024).expect("дыра закрыта — данные обязаны пойти наверх");
        assert_eq!(data[0], 7, "очередной сегмент отброшен пределом → вечный тупик");
    }

    #[test]
    fn unordered_bytes_counter_stays_in_sync() {
        // Счётчик — источник правды для приёмного окна; рассинхрон = либо
        // вечно нулевое окно (столл), либо неограниченный буфер (OOM).
        let mut tcb = tcb_with_read_buffer(64 * 1024);
        tcb.add_unordered_packet(SeqNum(1000), vec![1; 500]);
        tcb.add_unordered_packet(SeqNum(1500), vec![2; 500]);
        assert_eq!(tcb.get_unordered_packets_total_len(), 1000);
        tcb.consume_unordered_packets(700); // частичное потребление со сплитом
        assert_eq!(tcb.get_unordered_packets_total_len(), 300);
        assert_eq!(
            tcb.get_unordered_packets_total_len(),
            tcb.unordered_packets.values().map(|p| p.len()).sum::<usize>()
        );
        tcb.consume_unordered_packets(64 * 1024);
        assert_eq!(tcb.get_unordered_packets_total_len(), 0);
    }

    #[test]
    fn recv_window_closes_when_upstream_is_backlogged() {
        // BeMyVPN fork: раньше окно НИКОГДА не закрывалось (`avail.max(mtu)`),
        // и байты, уже отданные наверх, в нём не учитывались вовсе — притормозить
        // быстрого гостя было нечем, очередь наверх (unbounded) росла без границ.
        let mut tcb = tcb_with_read_buffer(16 * 1024);
        assert!(tcb.get_recv_window() > 0, "пустой буфер → окно открыто");
        tcb.reserve_upstream_queued(16 * 1024); // приложение не читает
        assert_eq!(tcb.get_recv_window(), 0, "очередь наверх забита, а окно всё ещё открыто");
        tcb.release_upstream_queued(16 * 1024); // приложение прочитало
        assert!(tcb.get_recv_window() > 0, "окно обязано открыться обратно");
    }

    #[test]
    fn window_scaling_multiplies_peer_window_and_divides_ours() {
        // ГЛАВНАЯ фича форка. Ошибка на единицу в сдвиге молча делит скорость
        // вдвое, поэтому проверяем обе стороны масштаба явными числами.
        let mut tcb = tcb_with_read_buffer(1024 * 1024);
        assert!(!tcb.is_wscale_negotiated());
        tcb.update_send_window(1000);
        assert_eq!(tcb.get_send_window(), 1000, "без wscale окно пира = сырое поле");

        tcb.set_window_shifts(7, 5);
        assert!(tcb.is_wscale_negotiated());
        assert_eq!(tcb.get_rcv_wnd_shift(), 5);
        assert_eq!(tcb.get_send_window(), 1000 << 7, "окно пира = поле × 2^snd_shift");

        // Наше окно: 1 МБ при сдвиге 5 = поле 32768, и оно ВЛЕЗАЕТ в u16.
        assert_eq!(tcb.get_recv_window() as usize, (1024 * 1024) >> 5);
        assert_eq!((tcb.get_recv_window() as usize) << 5, 1024 * 1024, "масштаб теряет байты приёмного буфера");

        // RFC 7323: сдвиг больше 14 запрещён — обязаны зажимать.
        tcb.set_window_shifts(30, 30);
        assert_eq!(tcb.get_rcv_wnd_shift(), 14);
    }

    #[test]
    fn check_pkt_type_across_seq_wraparound() {
        // Переход через 2^32: сравнения seq/ack модульные, и «меньше» рядом с
        // границей должно означать «раньше», а не «астрономически больше».
        let mut tcb = tcb_with_read_buffer(64 * 1024);
        tcb.seq = SeqNum(u32::MAX - 5);
        tcb.ack = SeqNum(u32::MAX - 5);
        tcb.last_received_ack = SeqNum(u32::MAX - 5);
        tcb.add_inflight_packet(vec![0u8; 20]).unwrap(); // seq уходит за 2^32 → 14

        let mk = |ack: u32, seq: u32| {
            let mut h = TcpHeader::new(1, 2, seq, 100);
            h.acknowledgment_number = ack;
            h.ack = true;
            h
        };

        // ACK «за оборотом» подтверждает данные, отправленные ДО оборота.
        assert_eq!(tcb.check_pkt_type(&mk(14, 0), &[]), PacketType::Ack);
        // Данные с seq за оборотом — новый пакет, а не мусор.
        assert_eq!(tcb.check_pkt_type(&mk(14, u32::MAX - 5), &[1, 2, 3]), PacketType::NewPacket);
        // ACK на то, чего мы не отправляли (за пределом seq) — невалиден.
        assert_eq!(tcb.check_pkt_type(&mk(100, 0), &[]), PacketType::Invalid);
        // Устаревший ACK до оборота — невалиден.
        assert_eq!(tcb.check_pkt_type(&mk(u32::MAX - 100, 0), &[]), PacketType::Invalid);
        // Keep-alive: seq = ack-1 (за оборотом), нагрузки не больше байта,
        // ACK повторяет последний виденный.
        assert_eq!(tcb.check_pkt_type(&mk(u32::MAX - 5, u32::MAX - 6), &[0]), PacketType::KeepAlive);
    }

    #[test]
    fn test_retransmit_with_exponential_backoff() {
        let mut tcb = Tcb::new(
            SeqNum(1000),
            1500,
            MAX_UNACK,
            READ_BUFFER_SIZE,
            MAX_COUNT_FOR_DUP_ACK,
            RTO,
            MAX_RETRANSMIT_COUNT,
        );

        tcb.add_inflight_packet(vec![1; 500]).unwrap();

        // Simulate retransmission timeouts
        for i in 0..MAX_RETRANSMIT_COUNT {
            // Simulate a timeout for the first packet
            let timeout = tcb.inflight_packets.values().next().unwrap().retransmit_timeout + std::time::Duration::from_millis(100);
            println!("timeout: {timeout:?}");
            std::thread::sleep(timeout);

            let packets = tcb.collect_timed_out_inflight_packets();
            assert_eq!(packets.len(), 1);
            let packet = &packets[0];
            assert_eq!(packet.retransmit_count, i + 1);
            assert!(packet.retransmit_timeout > RTO);
        }

        let packets = tcb.collect_timed_out_inflight_packets();
        assert!(packets.is_empty());
        assert!(tcb.inflight_packets.is_empty());
    }
}
