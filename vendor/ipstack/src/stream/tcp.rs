use super::seqnum::SeqNum;
use crate::{
    PacketReceiver, PacketSender, TTL,
    error::IpStackError,
    packet::{
        IpHeader, NetworkPacket, NetworkTuple, TransportHeader,
        tcp_flags::{ACK, FIN, PSH, RST, SYN},
        tcp_header_flags, tcp_header_fmt,
    },
    stream::tcb::{MAX_COUNT_FOR_DUP_ACK, MAX_RETRANSMIT_COUNT, MAX_UNACK, PacketType, READ_BUFFER_SIZE, RTO, Tcb, TcpState},
};
use etherparse::{IpNumber, Ipv4Header, Ipv6FlowLabel, TcpHeader, TcpOptionElement};
use std::{
    future::Future,
    io::ErrorKind::{BrokenPipe, ConnectionRefused, InvalidInput, UnexpectedEof},
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};

/// 2 * MSL (Maximum Segment Lifetime) is the maximum time a TCP connection can be in the TIME_WAIT state.
const TWO_MSL: Duration = Duration::from_secs(2);

const CLOSE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const LAST_ACK_MAX_RETRIES: usize = 3;
const LAST_ACK_TIMEOUT: Duration = Duration::from_millis(500);
// BeMyVPN fork: было 60с — простаивающее, но ЖИВОЕ соединение (HTTP/1.1
// keep-alive между запросами, пул сокетов, буферизованное видео на паузе,
// IMAP/websocket с редким heartbeat) принудительно закрывалось на 60-й секунде
// («session timeout reached, closing forcefully») → лишние реконнекты и
// TLS-рукопожатия, «страницы подвисают». 300с ≈ типичный TCP-idle у NAT/роутеров.
const TIMEOUT: Duration = Duration::from_secs(300);

#[non_exhaustive]
#[derive(Debug, Clone)]
/// TCP configuration
pub struct TcpConfig {
    /// Maximum number of retries for sending the last ACK in the LAST_ACK state. Default is 3.
    pub last_ack_max_retries: usize,
    /// Timeout for the last ACK in the LAST_ACK state. Default is 500ms.
    pub last_ack_timeout: Duration,
    /// Timeout for the CLOSE_WAIT state. Default is 5 seconds.
    pub close_wait_timeout: Duration,
    /// Timeout for TCP connections. Default is 60 seconds.
    pub timeout: Duration,
    /// Timeout for the TIME_WAIT state. Default is 2 seconds.
    pub two_msl: Duration,
    /// Maximum number of unacknowledged bytes allowed in the send buffer.
    pub max_unacked_bytes: u32,
    /// Size of the read buffer for incoming data.
    pub read_buffer_size: usize,
    /// Maximum number of duplicate ACKs before triggering fast retransmission.
    pub max_count_for_dup_ack: usize,
    /// Retransmission timeout duration.
    pub rto: std::time::Duration,
    /// Maximum number of retransmissions before giving up.
    pub max_retransmit_count: usize,
    /// TCP options
    pub options: Option<Vec<TcpOptions>>,
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum TcpOptions {
    /// Maximum segment size (MSS) for TCP connections.
    MaximumSegmentSize(u16),
    /// BeMyVPN fork: window scale (RFC 7323) — сдвиг нашего приёмного окна.
    WindowScale(u8),
}

impl Default for TcpConfig {
    fn default() -> Self {
        TcpConfig {
            last_ack_max_retries: LAST_ACK_MAX_RETRIES,
            last_ack_timeout: LAST_ACK_TIMEOUT,
            close_wait_timeout: CLOSE_WAIT_TIMEOUT,
            timeout: TIMEOUT,
            two_msl: TWO_MSL,
            max_unacked_bytes: MAX_UNACK,
            read_buffer_size: READ_BUFFER_SIZE,
            max_count_for_dup_ack: MAX_COUNT_FOR_DUP_ACK,
            rto: RTO,
            max_retransmit_count: MAX_RETRANSMIT_COUNT,
            options: Default::default(),
        }
    }
}

#[derive(Debug)]
enum Shutdown {
    None,
    Pending(Waker),
    Ready,
}

impl Shutdown {
    fn pending(&mut self, w: Waker) {
        *self = Shutdown::Pending(w);
    }
    fn ready(&mut self) {
        if let Shutdown::Pending(w) = self {
            w.wake_by_ref();
        }
        *self = Shutdown::Ready;
    }

    // Just for comparison purpose
    fn fake_clone(&self) -> Shutdown {
        match self {
            Shutdown::None => Shutdown::None,
            Shutdown::Pending(_) => Shutdown::Pending(Waker::noop().clone()),
            Shutdown::Ready => Shutdown::Ready,
        }
    }
}

impl std::fmt::Display for Shutdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Shutdown::None => write!(f, "None"),
            Shutdown::Pending(_) => write!(f, "Pending"),
            Shutdown::Ready => write!(f, "Ready"),
        }
    }
}

static SESSION_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

type TcbPtr = std::sync::Arc<std::sync::Mutex<Tcb>>;

/// A TCP stream in the IP stack.
///
/// This type represents a TCP connection and implements `AsyncRead` and `AsyncWrite`
/// for bidirectional data transfer. It handles TCP state management, flow control,
/// and retransmission automatically.
///
/// # Examples
///
/// ```no_run
/// use ipstack::{IpStack, IpStackConfig, IpStackStream};
/// use tokio::io::{AsyncReadExt, AsyncWriteExt};
///
/// # async fn example(mut ip_stack: IpStack) -> Result<(), Box<dyn std::error::Error>> {
/// if let IpStackStream::Tcp(mut tcp_stream) = ip_stack.accept().await? {
///     println!("New TCP connection from {}", tcp_stream.peer_addr());
///     
///     // Read data
///     let mut buffer = [0u8; 1024];
///     let n = tcp_stream.read(&mut buffer).await?;
///     
///     // Write data
///     tcp_stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await?;
///     
///     // Shutdown the stream
///     tcp_stream.shutdown().await?;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct IpStackTcpStream {
    src_addr: SocketAddr,
    dst_addr: SocketAddr,
    stream_sender: PacketSender,
    stream_receiver: Option<PacketReceiver>,
    up_packet_sender: PacketSender,
    tcb: TcbPtr,
    shutdown: std::sync::Arc<std::sync::Mutex<Shutdown>>,
    write_notify: std::sync::Arc<std::sync::Mutex<Option<Waker>>>,
    destroy_messenger: Option<::tokio::sync::oneshot::Sender<()>>,
    timeout: Pin<Box<tokio::time::Sleep>>,
    data_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    data_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    read_notify: std::sync::Arc<std::sync::Mutex<Option<Waker>>>,
    task_handle: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    exit_notifier: Option<tokio::sync::mpsc::Sender<()>>,
    temp_read_buffer: Vec<u8>,
    config: Arc<TcpConfig>,
}

impl IpStackTcpStream {
    pub(crate) fn new(
        src_addr: SocketAddr,
        dst_addr: SocketAddr,
        tcp: TcpHeader,
        up_packet_sender: PacketSender,
        mtu: u16,
        destroy_messenger: Option<::tokio::sync::oneshot::Sender<()>>,
        config: Arc<TcpConfig>,
    ) -> Result<IpStackTcpStream, IpStackError> {
        let mut tcb = Tcb::new(
            SeqNum(tcp.sequence_number),
            mtu,
            config.max_unacked_bytes,
            config.read_buffer_size,
            config.max_count_for_dup_ack,
            config.rto,
            config.max_retransmit_count,
        );
        // BeMyVPN fork: window scaling (RFC 7323). Если пир прислал wscale в
        // SYN — принимаем его сдвиг и отвечаем своим (иначе оба сдвига 0 и
        // поведение байт-в-байт как у оригинала). Свой сдвиг подбираем так,
        // чтобы приёмный буфер целиком влезал в 16-битное window-поле.
        if tcp.syn {
            let peer_shift = tcp.options_iterator().flatten().find_map(|o| match o {
                TcpOptionElement::WindowScale(s) => Some(s),
                _ => None,
            });
            if let Some(peer) = peer_shift {
                let mut ours: u8 = 0;
                while ours < 14 && (config.read_buffer_size >> ours) > u16::MAX as usize {
                    ours += 1;
                }
                tcb.set_window_shifts(peer, ours);
                log::debug!("wscale negotiated: peer shift {peer}, our shift {ours}");
            }
        }
        let tuple = NetworkTuple::new(src_addr, dst_addr, true);
        if !tcp.syn {
            if !tcp.rst
                && let Err(err) = write_packet_to_device(&up_packet_sender, tuple, &tcb, None, ACK | RST, None, None)
            {
                log::warn!("Error sending RST/ACK packet: {err}");
            }
            let info = format!("Invalid TCP packet: {tuple} {}", tcp_header_fmt(&tcp));
            return Err(IpStackError::IoError(std::io::Error::new(ConnectionRefused, info)));
        }

        let (stream_sender, stream_receiver) = tokio::sync::mpsc::unbounded_channel::<NetworkPacket>();
        let (data_tx, data_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let deadline = tokio::time::Instant::now() + config.timeout;

        let mut stream = IpStackTcpStream {
            src_addr,
            dst_addr,
            stream_sender,
            stream_receiver: Some(stream_receiver),
            up_packet_sender,
            tcb: std::sync::Arc::new(std::sync::Mutex::new(tcb.clone())),
            shutdown: std::sync::Arc::new(std::sync::Mutex::new(Shutdown::None)),
            write_notify: std::sync::Arc::new(std::sync::Mutex::new(None)),
            destroy_messenger,
            timeout: Box::pin(tokio::time::sleep_until(deadline)),
            data_tx,
            data_rx,
            read_notify: std::sync::Arc::new(std::sync::Mutex::new(None)),
            task_handle: None,
            exit_notifier: None,
            temp_read_buffer: Vec::new(),
            config,
        };

        let sessions = SESSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst).saturating_add(1);
        let (seq, ack, state) = { (tcb.get_seq().0, tcb.get_ack().0, tcb.get_state()) };
        let l_info = format!("local {{ seq: {seq}, ack: {ack} }}");
        log::debug!("{tuple} {state:?}: {l_info} session begins, total TCP sessions: {sessions}");

        stream.spawn_tasks()?;
        Ok(stream)
    }

    fn reset_timeout(&mut self) {
        let deadline = tokio::time::Instant::now() + self.config.timeout;
        self.timeout.as_mut().reset(deadline);
    }

    pub(crate) fn network_tuple(&self) -> NetworkTuple {
        NetworkTuple::new(self.src_addr, self.dst_addr, true)
    }

    /// Returns the local socket address of the TCP connection.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use ipstack::IpStackTcpStream;
    /// # fn example(tcp_stream: &IpStackTcpStream) {
    /// let local_addr = tcp_stream.local_addr();
    /// println!("Local address: {}", local_addr);
    /// # }
    /// ```
    pub fn local_addr(&self) -> SocketAddr {
        self.src_addr
    }

    /// Returns the remote socket address of the TCP connection.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use ipstack::IpStackTcpStream;
    /// # fn example(tcp_stream: &IpStackTcpStream) {
    /// let peer_addr = tcp_stream.peer_addr();
    /// println!("Peer address: {}", peer_addr);
    /// # }
    /// ```
    pub fn peer_addr(&self) -> SocketAddr {
        self.dst_addr
    }

    pub fn stream_sender(&self) -> PacketSender {
        self.stream_sender.clone()
    }
}

impl AsyncRead for IpStackTcpStream {
    fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        // if there is data in the temp buffer, read it first
        if !self.temp_read_buffer.is_empty() {
            let len = std::cmp::min(buf.remaining(), self.temp_read_buffer.len());
            buf.put_slice(&self.temp_read_buffer[..len]);
            self.temp_read_buffer.drain(..len); // remove the read data from the temp buffer
            return Poll::Ready(Ok(()));
        }

        let network_tuple = self.network_tuple();

        let state = self.tcb.lock().unwrap().get_state();
        if state == TcpState::Closed {
            self.shutdown.lock().unwrap().ready();
            self.write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
            return Poll::Ready(Ok(()));
        }

        // handle timeout
        if matches!(Pin::new(&mut self.timeout).poll(cx), Poll::Ready(_)) {
            {
                let mut tcb = self.tcb.lock().unwrap();
                let (seq, ack) = (tcb.get_seq().0, tcb.get_ack().0);
                let l_info = format!("local {{ seq: {seq}, ack: {ack} }}");
                log::warn!("{network_tuple} {state:?}: [poll_read] {l_info}, session timeout reached, closing forcefully...");
                let sender = &self.up_packet_sender;
                write_packet_to_device(sender, network_tuple, &tcb, None, ACK | RST, None, None)?;
                tcb.change_state(TcpState::Closed);
                let state = tcb.get_state();
                log::warn!("{network_tuple} {state:?}: [poll_read] {l_info}, session notified to close");
            }
            self.shutdown.lock().unwrap().ready();

            return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::TimedOut)));
        }
        self.reset_timeout();

        // read data from channel
        match self.data_rx.poll_recv(cx) {
            Poll::Ready(Some(data)) => {
                let len = data.len();
                let capacity = buf.remaining();
                if capacity >= len {
                    buf.put_slice(&data);
                } else {
                    // if `buf` is not enough, put the remaining data into the temp buffer
                    buf.put_slice(&data[..capacity]);
                    self.temp_read_buffer.extend_from_slice(&data[capacity..]);
                }
                // BeMyVPN fork: приложение забрало байты — место в приёмном буфере
                // вернулось, окно можно открыть. Если МЫ ЖЕ до этого объявили ноль,
                // пир сидит в persist-режиме и следующий зонд пришлёт через RTO с
                // экспоненциальным backoff (до минуты). Поэтому window update шлём
                // сами, сразу — иначе поток встаёт на секунды на ровном месте.
                let mut tcb = self.tcb.lock().unwrap();
                let was_closed = tcb.get_recv_window() == 0;
                tcb.release_upstream_queued(len);
                if was_closed && tcb.get_recv_window() > 0 {
                    write_packet_to_device(&self.up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                }
                drop(tcb);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Pending => {
                self.read_notify.lock().unwrap().replace(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl AsyncWrite for IpStackTcpStream {
    fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let nt = self.network_tuple();
        self.reset_timeout();

        let mut tcb = self.tcb.lock().unwrap();
        let state = tcb.get_state();
        let send_window = tcb.get_send_window();
        let is_full = tcb.is_send_buffer_full();

        if state == TcpState::Closed {
            self.shutdown.lock().unwrap().ready();
            self.read_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
            return Poll::Ready(Err(std::io::Error::new(BrokenPipe, "TCP connection closed")));
        }

        if send_window == 0 || is_full {
            self.write_notify.lock().unwrap().replace(cx.waker().clone());
            let info = format!("current send window: {send_window}, send buffer full: {is_full}");
            log::trace!("{nt} {state:?}: [poll_write] {info}, waiting for the other side to send ACK...");
            return Poll::Pending;
        }

        let sender = &self.up_packet_sender;
        // BeMyVPN fork: раньше здесь был buf.to_vec() — копия ВСЕГО буфера (до 64КБ,
        // столько даёт copy_bidirectional), хотя стек шлёт один сегмент (~MSS) за
        // вызов и остаток отбрасывает. Слив 64КБ по MSS-кускам = ~×23 лишнего
        // memcpy на горячем пути. Ограничиваем копию одним MTU.
        let cap = tcb.get_mtu() as usize;
        let seg = buf[..buf.len().min(cap)].to_vec();
        let payload_len = write_packet_to_device(sender, nt, &tcb, None, ACK | PSH, None, Some(seg))?;
        tcb.add_inflight_packet(buf[..payload_len].to_vec())?;

        let (state, seq, ack) = (tcb.get_state(), tcb.get_seq(), tcb.get_ack());
        let l_info = format!("local {{ seq: {seq}, ack: {ack} }}");
        log::trace!("{nt} {state:?}: [poll_write] {l_info} upstream data written to device, len = {payload_len}");

        Poll::Ready(Ok(payload_len))
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let shutdown = { self.shutdown.lock().unwrap().fake_clone() };
        let (nt, state, seq, is_ready) = {
            let tcb = self.tcb.lock().unwrap();
            let is_ready = tcb.get_inflight_packets_total_len() == 0;
            (self.network_tuple(), tcb.get_state(), tcb.get_seq(), is_ready)
        };
        log::trace!("{nt} {state:?}: [poll_shutdown] seq = {seq}, ready = {is_ready}, shutdown {shutdown}",);
        if state == TcpState::Closed {
            return Poll::Ready(Ok(()));
        }
        match shutdown {
            Shutdown::None => {
                if is_ready && state == TcpState::Established {
                    let mut tcb = self.tcb.lock().unwrap();
                    send_fin_n_change_state_to_fin_wait1("[poll_shutdown]", nt, &self.up_packet_sender, &mut tcb)?;
                }
                self.shutdown.lock().unwrap().pending(cx.waker().clone());
                Poll::Pending
            }
            Shutdown::Pending(_) => {
                if is_ready && state == TcpState::Established {
                    let mut tcb = self.tcb.lock().unwrap();
                    send_fin_n_change_state_to_fin_wait1("[poll_shutdown]", nt, &self.up_packet_sender, &mut tcb)?;
                }
                Poll::Pending
            }
            Shutdown::Ready => Poll::Ready(Ok(())),
        }
    }
}

fn send_fin_n_change_state_to_fin_wait1(hint: &str, nt: NetworkTuple, sender: &PacketSender, tcb: &mut Tcb) -> std::io::Result<()> {
    let state = tcb.get_state();
    if !(tcb.get_inflight_packets_total_len() == 0 && state == TcpState::Established) {
        log::debug!("{nt} {state:?}: {hint} session is not in a valid state to send FIN, skipping...");
        return Ok(());
    }

    log::debug!("{nt} {state:?}: {hint} actively send a farewell packet to the other side...");
    write_packet_to_device(sender, nt, tcb, None, ACK | FIN, None, None)?;
    tcb.increase_seq();
    tcb.change_state(TcpState::FinWait1);
    let state = tcb.get_state();
    log::debug!("{nt} {state:?}: {hint} now in {state:?} state");

    Ok(())
}

impl Drop for IpStackTcpStream {
    fn drop(&mut self) {
        let (nt, state) = (self.network_tuple(), self.tcb.lock().unwrap().get_state());
        log::trace!("{nt} {state:?}: [drop] session dropping, ========================= ");
        if let Some(task_handle) = self.task_handle.take() {
            if !task_handle.is_finished() {
                if let Some(notifier) = self.exit_notifier.take() {
                    _ = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(notifier.send(())));
                }
                // synchronously wait for the task to finish
                _ = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(task_handle));
            } else {
                log::trace!("{nt} {state:?}: [drop] task already finished, no need to wait exiting");
            }
        }
        let sessions = SESSION_COUNTER.fetch_sub(1, std::sync::atomic::Ordering::SeqCst).saturating_sub(1);
        log::debug!("{nt} {state:?}: [drop] session dropped, total TCP sessions: {sessions}");
    }
}

impl IpStackTcpStream {
    fn spawn_tasks(&mut self) -> std::io::Result<()> {
        let network_tuple = self.network_tuple();

        // task: data receiving and processing
        let tcb = self.tcb.clone();
        let stream_receiver = self.stream_receiver.take().unwrap();
        let up_packet_sender = self.up_packet_sender.clone();
        let shutdown = self.shutdown.clone();
        let write_notify = self.write_notify.clone();
        let read_notify = self.read_notify.clone();
        let data_tx = self.data_tx.clone();
        let destroy_messenger = self.destroy_messenger.take();

        let (exit_task_notifier, exit_monitor) = tokio::sync::mpsc::channel::<()>(10);
        let exit_notifier = exit_task_notifier.clone();
        let config = self.config.clone();
        self.exit_notifier = Some(exit_task_notifier);

        let task_handle = tokio::spawn(async move {
            let v = tcp_main_logic_loop(
                tcb,
                config,
                stream_receiver,
                up_packet_sender,
                exit_notifier,
                network_tuple,
                write_notify,
                read_notify,
                data_tx,
                exit_monitor,
            )
            .await;
            if let Err(e) = &v {
                log::warn!("{network_tuple} task error: {e}");
            }
            _ = destroy_messenger.map(|m| m.send(())).unwrap_or(Ok(()));
            log::trace!("{network_tuple} task completed, destroy messenger sent successfully");
            shutdown.lock().unwrap().ready();
            log::trace!("{network_tuple} shutdown.lock().unwrap().ready() ==========");
            v
        });
        self.task_handle = Some(task_handle);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn tcp_main_logic_loop(
    tcb: TcbPtr,
    config: Arc<TcpConfig>,
    mut stream_receiver: PacketReceiver,
    up_packet_sender: PacketSender,
    exit_notifier: tokio::sync::mpsc::Sender<()>,
    network_tuple: NetworkTuple,
    write_notify: std::sync::Arc<std::sync::Mutex<Option<Waker>>>,
    read_notify: std::sync::Arc<std::sync::Mutex<Option<Waker>>>,
    data_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    mut exit_monitor: tokio::sync::mpsc::Receiver<()>,
) -> std::io::Result<()> {
    {
        let mut tcb = tcb.lock().unwrap();

        let state = tcb.get_state();
        if state != TcpState::Listen {
            log::warn!("{network_tuple} {state:?}: Invalid TCP state, not in Listen state");
            return Ok::<(), std::io::Error>(());
        }

        tcb.increase_ack();
        let (seq, ack) = (tcb.get_seq().0, tcb.get_ack().0);
        let l_info = format!("local {{ seq: {seq}, ack: {ack} }}");
        log::trace!("{network_tuple} {state:?}: {l_info} session begins");
        // BeMyVPN fork: SYN-ACK всегда несёт MSS (иначе пир по RFC 1122 шлёт
        // сегменты по 536 байт — аплоад платит ×2.5 pps) и наш wscale, если
        // пир прислал свой (RFC 7323: масштаб включается только обоюдно).
        let mut synack_options: Vec<TcpOptions> = config.options.clone().unwrap_or_default();
        if !synack_options.iter().any(|o| matches!(o, TcpOptions::MaximumSegmentSize(_))) {
            synack_options.push(TcpOptions::MaximumSegmentSize(tcb.get_mtu().saturating_sub(40)));
        }
        if tcb.is_wscale_negotiated() {
            synack_options.push(TcpOptions::WindowScale(tcb.get_rcv_wnd_shift()));
        }
        write_packet_to_device(
            &up_packet_sender,
            network_tuple,
            &tcb,
            Some(&synack_options),
            ACK | SYN,
            None,
            None,
        )?;
        tcb.increase_seq();
        tcb.change_state(TcpState::SynReceived);
        let state = tcb.get_state();
        log::trace!("{network_tuple} {state:?}: session now in {state:?} state");
    }

    let tcb_clone = tcb.clone();

    async fn task_wait_to_close(tcb: TcbPtr, exit_notifier: tokio::sync::mpsc::Sender<()>, nt: NetworkTuple, two_msl: Duration) {
        tokio::time::sleep(two_msl).await;
        {
            let mut tcb = tcb.lock().unwrap();
            tcb.change_state(TcpState::Closed);
            let state = tcb.get_state();
            log::debug!("{nt} {state:?}: [task_wait_to_close] session closed after {two_msl:?}");
        }
        exit_notifier.send(()).await.unwrap_or(());
    }

    async fn task_last_ack(
        tcb: TcbPtr,
        exit_notifier: tokio::sync::mpsc::Sender<()>,
        nt: NetworkTuple,
        pkt_sdr: PacketSender,
        last_ack_timeout: Duration,
        last_ack_max_retries: usize,
    ) {
        let hint = "[task_last_ack]";
        for idx in 1..=last_ack_max_retries {
            let state = { tcb.lock().unwrap().get_state() };
            if state == TcpState::Closed {
                log::debug!("{nt} {state:?}: {hint} session closed, exiting 1...");
                return;
            }

            tokio::time::sleep(last_ack_timeout).await;

            {
                let tcb = tcb.lock().unwrap();
                let state = tcb.get_state();
                if state == TcpState::Closed {
                    log::debug!("{nt} {state:?}: {hint} session closed, exiting 2...");
                    return;
                }
                log::debug!("{nt} {state:?}: {hint} timer expired, resending ACK|FIN (retry {idx}/{last_ack_max_retries})");
                _ = write_packet_to_device(&pkt_sdr, nt, &tcb, None, ACK | FIN, None, None);
            }
        }
        {
            let mut tcb = tcb.lock().unwrap();
            tcb.change_state(TcpState::Closed);
            let state = tcb.get_state();
            log::warn!("{nt} {state:?}: {hint} max retries reached, forcibly closing session");
        }
        exit_notifier.send(()).await.unwrap_or(());
    }

    async fn task_timed_out_for_close_wait(
        tcb: TcbPtr,
        exit_notifier: tokio::sync::mpsc::Sender<()>,
        nt: NetworkTuple,
        up_packet_sender: PacketSender,
        close_wait_timeout: Duration,
        last_ack_timeout: Duration,
        last_ack_max_retries: usize,
    ) -> std::io::Result<()> {
        tokio::time::sleep(close_wait_timeout).await; // Wait CLOSE_WAIT_TIMEOUT for upstream
        let tcb_clone = tcb.clone();
        let mut tcb = tcb.lock().unwrap();
        let state = tcb.get_state();
        if state != TcpState::CloseWait {
            return Ok(());
        }
        log::warn!("{nt} {state:?}: Upstream timeout, forcing FIN");
        write_packet_to_device(&up_packet_sender, nt, &tcb, None, ACK | FIN, None, None)?;
        tcb.increase_seq();
        tcb.change_state(TcpState::LastAck);
        let new_state = tcb.get_state();
        log::debug!("{nt} {state:?}: Forced transition to {new_state:?}");

        // Here we set a timer to wait for the last ACK from the other side.
        tokio::spawn(task_last_ack(
            tcb_clone,
            exit_notifier,
            nt,
            up_packet_sender,
            last_ack_timeout,
            last_ack_max_retries,
        ));

        Ok::<(), std::io::Error>(())
    }

    // BeMyVPN fork: таймер ретрансмита. Оригинал проверял таймауты inflight
    // ТОЛЬКО при входящем пакете — если потеряно всё окно, пиру нечего ACK'ать,
    // входящих нет и ретрансмит не срабатывает никогда (вечный столл потока).
    // Периодический тик чинит это, работая как настоящий RTO-таймер.
    let mut rto_tick = tokio::time::interval(Duration::from_millis(100));
    rto_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let exit_notifier = exit_notifier.clone();

        let network_packet = tokio::select! {
            _ = exit_monitor.recv() => {
                log::debug!("{network_tuple} task exited due to exit signal");
                break;
            }
            _ = rto_tick.tick() => {
                let mut tcb = tcb.lock().unwrap();
                if tcb.get_state() == TcpState::Closed {
                    break;
                }
                for packet in tcb.collect_timed_out_inflight_packets() {
                    let (seq, count) = (packet.seq, packet.retransmit_count);
                    log::debug!("{network_tuple} RTO tick: retransmit seq {seq:?}, count {count}");
                    write_packet_to_device(
                        &up_packet_sender,
                        network_tuple,
                        &tcb,
                        None,
                        ACK | PSH,
                        Some(seq),
                        Some(packet.payload),
                    )?;
                }
                continue;
            }
            network_packet = stream_receiver.recv() => network_packet,
        };

        let Some(mut network_packet) = network_packet else {
            let state = { tcb.lock().unwrap().get_state() };
            log::debug!("{network_tuple} {state:?}: session closed unexpectedly by pipe broken, exiting task");
            tcb.lock().unwrap().change_state(TcpState::Closed);
            write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
            read_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
            break;
        };

        let payload = network_packet.payload.take().unwrap_or_default();
        let TransportHeader::Tcp(tcp_header) = network_packet.transport_header() else {
            log::warn!("{network_tuple} Invalid TCP packet");
            continue;
        };
        let flags = tcp_header_flags(tcp_header);
        let incoming_ack: SeqNum = tcp_header.acknowledgment_number.into();
        let incoming_seq: SeqNum = tcp_header.sequence_number.into();
        let incoming_win = tcp_header.window_size;

        let mut tcb = tcb.lock().unwrap();

        let state = tcb.get_state();
        if state == TcpState::Closed {
            log::debug!("{network_tuple} {state:?}: session finished, exiting task...");
            break;
        }

        if flags & RST == RST {
            tcb.change_state(TcpState::Closed);
            continue;
        }

        tcb.update_duplicate_ack_count(incoming_ack);

        tcb.update_inflight_packet_queue(incoming_ack);

        for packet in tcb.collect_timed_out_inflight_packets() {
            let (seq, count) = (packet.seq, packet.retransmit_count);
            log::debug!("{network_tuple} inflight packet retransmission timeout: {seq:?}, retransmit_count: {count}",);
            write_packet_to_device(
                &up_packet_sender,
                network_tuple,
                &tcb,
                None,
                ACK | PSH,
                Some(seq),
                Some(packet.payload),
            )?;
        }

        let pkt_type = tcb.check_pkt_type(tcp_header, &payload);

        let (state, seq, ack) = { (tcb.get_state(), tcb.get_seq(), tcb.get_ack()) };
        let (info, len) = (tcp_header_fmt(tcp_header), payload.len());
        let l_info = format!("local {{ seq: {seq}, ack: {ack} }}");
        log::trace!("{network_tuple} {state:?}: {l_info} {info}, {pkt_type:?}, len = {len}");
        if pkt_type == PacketType::Invalid {
            continue;
        }

        match state {
            TcpState::SynReceived => {
                if flags & ACK == ACK {
                    if len > 0 {
                        tcb.add_unordered_packet(incoming_seq, payload);
                        extract_data_n_write_upstream(&up_packet_sender, &mut tcb, network_tuple, &data_tx, &read_notify)?;
                    }
                    tcb.change_state(TcpState::Established);
                }
            }
            TcpState::Established => {
                // BeMyVPN fork: разбор по БИТОВЫМ МАСКАМ вместо точного сравнения
                // флагов. Раньше ветки писались как `flags == ACK` и
                // `flags == (ACK | FIN)`, а tcp_header_flags собирает все 8 бит:
                // реальный стек на `send(); close();` склеивает данные с FIN и
                // ставит PSH → приходил ACK|PSH|FIN, не совпадал НИ С ОДНОЙ веткой
                // и молча проваливался в «ничего не делаем». Данные терялись, FIN
                // не обрабатывался, соединение висело до таймаута сессии.
                // Порядок как в RFC 793 §3.9: RST (обработан выше по циклу) →
                // данные → FIN.
                if len > 0 {
                    if pkt_type == PacketType::KeepAlive {
                        write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                    } else {
                        // Кладём БЕЗУСЛОВНО, в том числе вне порядка: свою защиту
                        // (seq < ack и предел буфера) add_unordered_packet делает
                        // сам, а extract отдаст наверх ровно то, что собралось.
                        tcb.add_unordered_packet(incoming_seq, payload);
                        extract_data_n_write_upstream(&up_packet_sender, &mut tcb, network_tuple, &data_tx, &read_notify)?;
                    }
                    // Вместе с данными приехал ACK — окно пира могло сдвинуться.
                    write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
                } else {
                    match pkt_type {
                        PacketType::KeepAlive => {
                            write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                        }
                        PacketType::RetransmissionRequest => {
                            // BeMyVPN fork: снижаем cwnd и ретрансмитим ОДИН раз на
                            // потерю (fast recovery), а не на каждый из десятков
                            // dup-ACK'ов одного окна — иначе cwnd рушился в пол.
                            if tcb.enter_fast_recovery() {
                                if let Some(packet) = tcb.find_inflight_packet(incoming_ack) {
                                    let (s, p) = (packet.seq, packet.payload.clone());
                                    log::debug!(
                                        "{network_tuple} {state:?}: {l_info}, {pkt_type:?}, fast retransmit, seq = {s}, len = {}",
                                        p.len()
                                    );
                                    write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK | PSH, Some(s), Some(p))?;
                                }
                            }
                            // Окно пира могло сдвинуться вместе с dup-ACK — будим писателя.
                            write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
                        }
                        // WindowUpdate / Ack — просто разбудить писателя. NewPacket
                        // сюда не попадает (len == 0), Invalid отсеян выше.
                        _ => {
                            write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
                        }
                    }
                }

                // FIN обрабатываем ТОЛЬКО когда собраны все предшествующие данные:
                // иначе (внеочередной сегмент с FIN) мы бы подтвердили закрытие,
                // перепрыгнув дыру, и порвали поток. Дыра есть → шлём дубль-ACK,
                // пир быстро повторит потерянное и следом FIN.
                // `>=`, а не `==`: повторно приехавший сегмент «данные + FIN»
                // (мы уже приняли данные, но наш ACK потерялся) тоже должен
                // закрывать соединение, а не гонять лишний круг.
                let fin_in_order = tcb.get_ack() >= incoming_seq + len as u32;
                if flags & FIN == FIN && !fin_in_order {
                    write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                    log::debug!("{network_tuple} {state:?}: FIN пришёл вперёд данных, ждём дыру {}", tcb.get_ack());
                } else if flags & FIN == FIN {
                    // The other side is closing the connection, we need to send an ACK and change state to CloseWait
                    tcb.increase_ack();
                    write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                    tcb.change_state(TcpState::CloseWait);

                    let s = tcb.get_state();
                    let len = tcb.get_inflight_packets_total_len();
                    if len == 0 {
                        // All upstream data sent, proceed to LastAck
                        log::trace!("{network_tuple} {s:?}: {l_info}, {pkt_type:?}, closed by the other side, no upstream data");

                        // Here we don't wait, just send FIN to the other side and change state to LastAck directly,
                        write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK | FIN, None, None)?;
                        tcb.increase_seq();
                        tcb.change_state(TcpState::LastAck);

                        let s = tcb.get_state();
                        log::trace!("{network_tuple} {s:?}: {l_info}, {pkt_type:?}, wait the last ack from the other side");

                        // Here we set a timer to wait for the last ACK from the other side.
                        // If the timer expires, we send an ACK|FIN packet to the other side again and wait anthoer timeout
                        // till the retries reach the limit, and then close the session forcibly.
                        let up = up_packet_sender.clone();
                        tokio::spawn(task_last_ack(
                            tcb_clone.clone(),
                            exit_notifier,
                            network_tuple,
                            up,
                            config.last_ack_timeout,
                            config.last_ack_max_retries,
                        ));
                    } else {
                        // Upstream data pending, wake write_notify and wait
                        write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
                        log::debug!("{network_tuple} {state:?}: Waiting for upstream data to complete, inflight packets: {len}",);

                        // Spawn a timeout task to force FIN if upstream is unresponsive
                        let tcb = tcb_clone.clone();
                        let up = up_packet_sender.clone();
                        tokio::spawn(task_timed_out_for_close_wait(
                            tcb,
                            exit_notifier,
                            network_tuple,
                            up,
                            config.close_wait_timeout,
                            config.last_ack_timeout,
                            config.last_ack_max_retries,
                        ));
                    }
                }
            }
            TcpState::CloseWait => {
                if flags & ACK == ACK && tcb.get_inflight_packets_total_len() == 0 {
                    write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK | FIN, None, None)?;
                    tcb.increase_seq();
                    tcb.change_state(TcpState::LastAck);
                    let new_state = tcb.get_state();
                    log::trace!("{network_tuple} {state:?}: Received ACK|FIN, transitioned to {new_state:?}");

                    // Here we set a timer to wait for the last ACK from the other side.
                    // If the timer expires, we send an ACK|FIN packet to the other side again and wait anthoer timeout
                    // till the retries reach the limit, and then close the session forcibly.
                    let up = up_packet_sender.clone();
                    tokio::spawn(task_last_ack(
                        tcb_clone.clone(),
                        exit_notifier,
                        network_tuple,
                        up,
                        config.last_ack_timeout,
                        config.last_ack_max_retries,
                    ));
                } else {
                    write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
                }
            }
            TcpState::LastAck => {
                if flags & ACK == ACK {
                    tcb.change_state(TcpState::Closed);
                    tokio::spawn(async move {
                        if let Err(e) = exit_notifier.send(()).await {
                            log::debug!("exit_notifier send failed: {e}");
                        }
                    });
                    let new_state = tcb.get_state();
                    log::trace!("{network_tuple} {state:?}: Received final ACK, transitioned to {new_state:?}");
                }
            }
            TcpState::FinWait1 => {
                if flags & (ACK | FIN) == (ACK | FIN) && len == 0 {
                    // If the received packet is an ACK with FIN, we need to send an ACK and change state to TimeWait directly, not to FinWait2
                    tcb.increase_ack();
                    write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                    tcb.change_state(TcpState::TimeWait);

                    tokio::spawn(task_wait_to_close(tcb_clone.clone(), exit_notifier, network_tuple, config.two_msl));
                    let new_state = tcb.get_state();
                    log::trace!("{network_tuple} {state:?}: Final ACK|FIN received too early, transitioned to {new_state:?} directly");
                } else if flags & ACK == ACK {
                    tcb.change_state(TcpState::FinWait2);
                    if len > 0 {
                        // if the other side is still sending data, we need to deal with it like PacketStatus::NewPacket
                        tcb.add_unordered_packet(incoming_seq, payload);
                        extract_data_n_write_upstream(&up_packet_sender, &mut tcb, network_tuple, &data_tx, &read_notify)?;
                        write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
                    }
                    let new_state = tcb.get_state();
                    log::trace!("{network_tuple} {state:?}: Received ACK, transitioned to {new_state:?}");
                } else {
                    // unnormal case, we do nothing here
                    log::trace!("{network_tuple} {state:?}: Some unnormal case, we do nothing here");
                }
            }
            TcpState::FinWait2 => {
                if flags & (ACK | FIN) == (ACK | FIN) && len == 0 {
                    tcb.increase_ack();
                    write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                    tcb.change_state(TcpState::TimeWait);
                    tokio::spawn(task_wait_to_close(tcb_clone.clone(), exit_notifier, network_tuple, config.two_msl));
                    let new_state = tcb.get_state();
                    log::trace!("{network_tuple} {state:?}: Received final ACK|FIN, transitioned to {new_state:?}");
                } else if flags & ACK == ACK && len == 0 {
                    // unnormal case, we do nothing here
                    let l_ack = tcb.get_ack();
                    if incoming_seq < l_ack {
                        log::trace!("{network_tuple} {state:?}: Ignoring duplicate ACK, seq {incoming_seq}, expected {l_ack}");
                    }
                } else if flags & ACK == ACK && len > 0 {
                    if pkt_type == PacketType::KeepAlive {
                        write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                    } else {
                        // if the other side is still sending data, we need to deal with it like PacketStatus::NewPacket
                        tcb.add_unordered_packet(incoming_seq, payload);
                        extract_data_n_write_upstream(&up_packet_sender, &mut tcb, network_tuple, &data_tx, &read_notify)?;
                        write_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
                    }
                    if flags & FIN == FIN {
                        tcb.change_state(TcpState::TimeWait);
                        tokio::spawn(task_wait_to_close(tcb_clone.clone(), exit_notifier, network_tuple, config.two_msl));
                        let new_state = tcb.get_state();
                        log::trace!("{network_tuple} {state:?}: Received final ACK|FIN, transitioned to {new_state:?}");
                    }
                } else {
                    // unnormal case, we do nothing here
                    log::trace!("{network_tuple} {state:?}: Some unnormal case, we do nothing here");
                }
            }
            TcpState::TimeWait => {
                if flags & (ACK | FIN) == (ACK | FIN) {
                    write_packet_to_device(&up_packet_sender, network_tuple, &tcb, None, ACK, None, None)?;
                    // wait to timeout, can't call `tcb.change_state(TcpState::Closed);` to change state here
                    // now we need to wait for the timeout to reach...
                }
            }
            _ => {}
        } // end of match state

        tcb.update_last_received_ack(incoming_ack);
        tcb.update_send_window(incoming_win);
    } // end of loop
    Ok::<(), std::io::Error>(())
}

fn extract_data_n_write_upstream(
    up_packet_sender: &PacketSender,
    tcb: &mut Tcb,
    network_tuple: NetworkTuple,
    data_tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    read_notify: &std::sync::Arc<std::sync::Mutex<Option<Waker>>>,
) -> std::io::Result<()> {
    let (state, seq, ack) = (tcb.get_state(), tcb.get_seq(), tcb.get_ack());
    let l_info = format!("local {{ seq: {seq}, ack: {ack} }}");
    if state == TcpState::Closed {
        log::debug!("{network_tuple} {state:?}: {l_info} session closed, exiting \"data extraction task\"...");
        return Ok(());
    }

    // BeMyVPN fork: тянем ВСЁ, что уже собралось по порядку. Раньше был один
    // вызов consume(8192): если дыру закрывал ретрансмит и за ней лежало больше
    // 8 КБ, хвост оставался в буфере до следующего ВХОДЯЩЕГО пакета — лишний
    // круг RTT на ровном месте.
    let mut delivered = false;
    while let Some(data) = tcb.consume_unordered_packets(8192) {
        let hint = if state == TcpState::Established { "normally" } else { "still" };
        log::trace!("{network_tuple} {state:?}: {l_info} {hint} receiving data, len = {}", data.len());
        // Байты уходят в НЕограниченный канал наверх — учитываем их в приёмном
        // окне, иначе объявленное окно врёт и тормозить пира нечем.
        tcb.reserve_upstream_queued(data.len());
        data_tx.send(data).map_err(|e| std::io::Error::new(BrokenPipe, e))?;
        delivered = true;
    }
    if delivered {
        read_notify.lock().unwrap().take().map(|w| w.wake_by_ref()).unwrap_or(());
        write_packet_to_device(up_packet_sender, network_tuple, tcb, None, ACK, None, None)?;
    } else if tcb.get_unordered_packets_total_len() > 0 {
        // BeMyVPN fork: данные пришли ВНЕ ПОРЯДКА (дыра на self.ack) — шлём
        // немедленный ДУБЛЬ-ACK текущего ack. Раньше здесь молчали → отправитель
        // (реальный стек гостя на upload) не получал 3 dup-ACK и ждал СВОЙ RTO
        // вместо fast-retransmit. Это «ACK на каждый сегмент вне порядка» из RFC.
        write_packet_to_device(up_packet_sender, network_tuple, tcb, None, ACK, None, None)?;
    }
    Ok(())
}

/// Send a TCP packet to the downstream device, with the specified flags, sequence number, and payload.
/// The returned value is the length of the `payload` sent, it may be shorter than the length of the incoming parameter `payload`.
pub(crate) fn write_packet_to_device(
    up_packet_sender: &PacketSender,
    tuple: NetworkTuple,
    tcb: &Tcb,
    options: Option<&Vec<TcpOptions>>,
    flags: u8,
    seq: Option<SeqNum>,
    payload: Option<Vec<u8>>,
) -> std::io::Result<usize> {
    use std::io::Error;
    let seq = seq.unwrap_or(tcb.get_seq()).0;
    // BeMyVPN fork: get_recv_window уже включает floor по MTU и масштабирование
    // wscale — внешний .max(mtu) сломал бы масштаб (сравнил бы делённое поле с
    // сырым MTU), поэтому убран.
    let (ack, window_size) = (tcb.get_ack().0, tcb.get_recv_window());
    let (src, dst) = (tuple.dst, tuple.src); // Note: The address is reversed here
    let calc = |ip_header_len: usize, tcp_header_len: usize| tcb.calculate_payload_max_len(ip_header_len, tcp_header_len);
    let packet = create_raw_packet(
        src,
        dst,
        calc,
        flags,
        TTL,
        seq,
        ack,
        window_size,
        payload.unwrap_or_default(),
        options,
    )?;
    let len = packet.payload.as_ref().map(|p| p.len()).unwrap_or(0);
    up_packet_sender.send(packet).map_err(|e| Error::new(UnexpectedEof, e))?;
    Ok(len)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_raw_packet(
    src_addr: SocketAddr,
    dst_addr: SocketAddr,
    calculate_payload_max_len: impl Fn(usize, usize) -> usize,
    flags: u8,
    ttl: u8,
    seq: u32,
    ack: u32,
    win: u16,
    mut payload: Vec<u8>,
    options: Option<&Vec<TcpOptions>>,
) -> std::io::Result<NetworkPacket> {
    let mut tcp_header = etherparse::TcpHeader::new(src_addr.port(), dst_addr.port(), seq, win);
    tcp_header.acknowledgment_number = ack;
    tcp_header.syn = flags & SYN != 0;
    tcp_header.ack = flags & ACK != 0;
    tcp_header.rst = flags & RST != 0;
    tcp_header.fin = flags & FIN != 0;
    tcp_header.psh = flags & PSH != 0;

    if let Some(opts) = options {
        let mut tcp_options = Vec::new();
        for opt in opts {
            match opt {
                TcpOptions::MaximumSegmentSize(mss) => tcp_options.push(TcpOptionElement::MaximumSegmentSize(*mss)),
                TcpOptions::WindowScale(shift) => tcp_options.push(TcpOptionElement::WindowScale(*shift)),
            }
        }
        tcp_header
            .set_options(&tcp_options)
            .map_err(|e| std::io::Error::new(InvalidInput, e))?;
    }
    let ip_header = match (src_addr.ip(), dst_addr.ip()) {
        (std::net::IpAddr::V4(src), std::net::IpAddr::V4(dst)) => {
            let mut ip_h =
                Ipv4Header::new(0, ttl, IpNumber::TCP, src.octets(), dst.octets()).map_err(|e| std::io::Error::new(InvalidInput, e))?;
            let payload_len = calculate_payload_max_len(ip_h.header_len(), tcp_header.header_len());
            payload.truncate(payload_len);
            ip_h.set_payload_len(payload.len() + tcp_header.header_len())
                .map_err(|e| std::io::Error::new(InvalidInput, e))?;
            ip_h.dont_fragment = true;
            IpHeader::Ipv4(ip_h)
        }
        (std::net::IpAddr::V6(src), std::net::IpAddr::V6(dst)) => {
            let mut ip_h = etherparse::Ipv6Header {
                traffic_class: 0,
                flow_label: Ipv6FlowLabel::ZERO,
                payload_length: 0,
                next_header: IpNumber::TCP,
                hop_limit: ttl,
                source: src.octets(),
                destination: dst.octets(),
            };
            let payload_len = calculate_payload_max_len(ip_h.header_len(), tcp_header.header_len());
            payload.truncate(payload_len);
            let len = payload.len() + tcp_header.header_len();
            ip_h.set_payload_length(len).map_err(|e| std::io::Error::new(InvalidInput, e))?;

            IpHeader::Ipv6(ip_h)
        }
        _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "IP version mismatch")),
    };

    match ip_header {
        IpHeader::Ipv4(ref ip_header) => {
            tcp_header.checksum = tcp_header
                .calc_checksum_ipv4(ip_header, &payload)
                .map_err(|e| std::io::Error::new(InvalidInput, e))?;
        }
        IpHeader::Ipv6(ref ip_header) => {
            tcp_header.checksum = tcp_header
                .calc_checksum_ipv6(ip_header, &payload)
                .map_err(|e| std::io::Error::new(InvalidInput, e))?;
        }
    }
    Ok(NetworkPacket {
        ip: ip_header,
        transport: TransportHeader::Tcp(tcp_header),
        payload: Some(payload),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    const GUEST: &str = "10.0.0.2:40000";
    const SERVER: &str = "1.1.1.1:80";
    const PEER_ISN: u32 = 5000;
    /// В debug-сборке Tcb::new берёт фиксированный ISN 100, +1 за SYN-ACK.
    const OUR_SEQ: u32 = 101;

    fn addrs() -> (SocketAddr, SocketAddr) {
        (GUEST.parse().unwrap(), SERVER.parse().unwrap())
    }

    /// Поднять настоящий `IpStackTcpStream` (со своей фоновой задачей) и отдать
    /// приёмник исходящих пакетов — то есть встать на место «провода».
    fn new_stream(config: TcpConfig) -> (IpStackTcpStream, PacketReceiver) {
        let (guest, server) = addrs();
        let (up_tx, up_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut syn = TcpHeader::new(guest.port(), server.port(), PEER_ISN, 64000);
        syn.syn = true;
        let s = IpStackTcpStream::new(guest, server, syn, up_tx, 1500, None, Arc::new(config)).unwrap();
        (s, up_rx)
    }

    /// Отправить стеку пакет «от гостя».
    fn send(stream: &IpStackTcpStream, flags: u8, seq: u32, payload: &[u8]) {
        send_with_ack(stream, flags, seq, OUR_SEQ, payload)
    }

    fn send_with_ack(stream: &IpStackTcpStream, flags: u8, seq: u32, ack: u32, payload: &[u8]) {
        let (guest, server) = addrs();
        let p = create_raw_packet(guest, server, |_, _| 1500, flags, 64, seq, ack, 64000, payload.to_vec(), None).unwrap();
        stream.stream_sender().send(p).unwrap();
    }

    async fn next_pkt(rx: &mut PacketReceiver) -> Option<(TcpHeader, Vec<u8>)> {
        let p = tokio::time::timeout(Duration::from_millis(400), rx.recv()).await.ok()??;
        let TransportHeader::Tcp(h) = p.transport else { return None };
        Some((h, p.payload.unwrap_or_default()))
    }

    /// Собрать все исходящие пакеты, пока стек не замолчит.
    async fn drain(rx: &mut PacketReceiver) -> Vec<(TcpHeader, Vec<u8>)> {
        let mut out = Vec::new();
        while let Some(p) = next_pkt(rx).await {
            out.push(p);
        }
        out
    }

    async fn handshake(stream: &IpStackTcpStream, rx: &mut PacketReceiver) {
        let (synack, _) = next_pkt(rx).await.expect("нет SYN-ACK");
        assert!(synack.syn && synack.ack);
        assert_eq!(synack.acknowledgment_number, PEER_ISN + 1);
        send(stream, ACK, PEER_ISN + 1, b""); // финальный ACK рукопожатия
    }

    async fn read_some(stream: &mut IpStackTcpStream) -> Vec<u8> {
        let mut buf = [0u8; 4096];
        match tokio::time::timeout(Duration::from_millis(400), stream.read(&mut buf)).await {
            Ok(Ok(n)) => buf[..n].to_vec(),
            _ => Vec::new(),
        }
    }

    /// Точка 7: подбор нашего wscale в SYN-ACK — ГЛАВНАЯ фича форка. Ошибка на
    /// единицу в сдвиге молча делит скорость надвое, а пропущенная опция
    /// выключает масштаб для ОБЕИХ сторон (RFC 7323 §2.2).
    #[tokio::test(flavor = "multi_thread")]
    async fn syn_ack_carries_mss_and_minimal_sufficient_window_scale() {
        const BUF: usize = 1024 * 1024;
        let (guest, server) = addrs();
        let mut config = TcpConfig::default();
        config.read_buffer_size = BUF;

        let (up_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut syn = TcpHeader::new(guest.port(), server.port(), PEER_ISN, 64000);
        syn.syn = true;
        syn.set_options(&[TcpOptionElement::WindowScale(7)]).unwrap();
        let _s = IpStackTcpStream::new(guest, server, syn, up_tx, 1500, None, Arc::new(config.clone())).unwrap();

        let (h, _) = next_pkt(&mut rx).await.expect("нет SYN-ACK");
        assert!(h.syn && h.ack);
        let opt = |h: &TcpHeader| {
            let (mut ws, mut mss) = (None, None);
            for o in h.options_iterator().flatten() {
                match o {
                    TcpOptionElement::WindowScale(s) => ws = Some(s),
                    TcpOptionElement::MaximumSegmentSize(m) => mss = Some(m),
                    _ => {}
                }
            }
            (ws, mss)
        };
        let (ws, mss) = opt(&h);
        assert_eq!(mss, Some(1500 - 40), "без MSS пир уйдёт на 536 байт/сегмент");
        let shift = ws.expect("SYN-ACK без window scale — масштаб выключается для обеих сторон");

        // Сдвиг обязан быть МИНИМАЛЬНЫМ достаточным: буфер целиком влезает в
        // 16-битное поле, а на единицу меньше — уже нет. Больше нужного = грубее
        // гранулярность, меньше = теряем половину окна, то есть половину скорости.
        assert_eq!((BUF >> shift) as usize, u16::from(h.window_size) as usize);
        assert!(BUF >> shift <= u16::MAX as usize, "сдвиг мал: окно не влезает в поле");
        assert!(BUF >> (shift - 1) > u16::MAX as usize, "сдвиг великоват, можно было точнее");
        assert_eq!((h.window_size as usize) << shift, BUF, "объявленное окно не покрывает приёмный буфер");

        // Пир БЕЗ wscale: опции быть не должно, поле зажимается в u16 — иначе
        // объявили бы мусор от переполнения.
        let (up_tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        let mut plain = TcpHeader::new(guest.port(), server.port(), PEER_ISN, 64000);
        plain.syn = true;
        let _s2 = IpStackTcpStream::new(guest, server, plain, up_tx2, 1500, None, Arc::new(config)).unwrap();
        let (h2, _) = next_pkt(&mut rx2).await.expect("нет SYN-ACK");
        let (ws2, _) = opt(&h2);
        assert_eq!(ws2, None, "мы не имеем права включать масштаб в одностороннем порядке");
        assert_eq!(h2.window_size, u16::MAX, "без масштаба окно должно упираться в потолок поля");
    }

    /// Страховка на рефактор горячего пути: обычный поток по порядку в обе
    /// стороны и штатное закрытие обязаны работать ровно как раньше.
    #[tokio::test(flavor = "multi_thread")]
    async fn plain_in_order_transfer_and_graceful_close() {
        use tokio::io::AsyncWriteExt;
        let (mut stream, mut rx) = new_stream(TcpConfig::default());
        handshake(&stream, &mut rx).await;

        let mut seq = PEER_ISN + 1;
        for i in 0..3u8 {
            send(&stream, ACK | PSH, seq, &[i; 1000]);
            seq += 1000;
        }
        let mut got = Vec::new();
        while got.len() < 3000 {
            let chunk = read_some(&mut stream).await;
            if chunk.is_empty() {
                break;
            }
            got.extend_from_slice(&chunk);
        }
        assert_eq!(got.len(), 3000, "обычный поток по порядку сломан");
        assert_eq!((got[0], got[1500], got[2999]), (0, 1, 2), "куски перепутаны местами");
        let acks: Vec<u32> = drain(&mut rx).await.iter().map(|(h, _)| h.acknowledgment_number).collect();
        assert!(acks.contains(&seq), "нет ACK на конец потока, ack'и: {acks:?}");

        // Данные вниз: приложение пишет — сегмент обязан уйти на провод.
        stream.write_all(b"hello").await.unwrap();
        let (h, payload) = next_pkt(&mut rx).await.expect("ответ приложения не ушёл на провод");
        assert!(h.ack && h.psh);
        assert_eq!(payload, b"hello");

        // Пир подтверждает наши 5 байт и закрывается штатным FIN без данных.
        send_with_ack(&stream, ACK | FIN, seq, OUR_SEQ + 5, b"");
        let pkts = drain(&mut rx).await;
        assert!(pkts.iter().any(|(h, _)| h.acknowledgment_number == seq + 1), "FIN не подтверждён");
        assert!(pkts.iter().any(|(h, _)| h.fin), "стек не ответил своим FIN");
    }

    /// Keep-alive приходит с PSH так же часто, как обычные данные. Раньше такой
    /// зонд не совпадал ни с одной веткой и оставался БЕЗ ОТВЕТА — пир считал
    /// соединение мёртвым и рвал его.
    #[tokio::test(flavor = "multi_thread")]
    async fn keep_alive_probe_is_answered_and_not_leaked_upstream() {
        let (mut stream, mut rx) = new_stream(TcpConfig::default());
        handshake(&stream, &mut rx).await;

        send(&stream, ACK | PSH, PEER_ISN, &[0xFF]); // seq = ack-1, один байт
        let (h, payload) = next_pkt(&mut rx).await.expect("keep-alive остался без ответа");
        assert!(h.ack && payload.is_empty());
        assert_eq!(h.acknowledgment_number, PEER_ISN + 1, "ack не должен двигаться от keep-alive");
        assert!(read_some(&mut stream).await.is_empty(), "байт keep-alive утёк в поток приложения");
    }

    /// Точка 1: сегмент «данные + закрытие». Реальные стеки на `send(); close();`
    /// склеивают данные с FIN и ставят PSH → приходит ACK|PSH|FIN. Точное
    /// сравнение `flags == (ACK | FIN)` его не ловило: данные терялись, FIN не
    /// обрабатывался, соединение висело до таймаута.
    #[tokio::test(flavor = "multi_thread")]
    async fn data_with_psh_and_fin_is_delivered_and_closed() {
        let (mut stream, mut rx) = new_stream(TcpConfig::default());
        handshake(&stream, &mut rx).await;

        send(&stream, ACK | PSH | FIN, PEER_ISN + 1, b"bye");

        assert_eq!(read_some(&mut stream).await, b"bye", "данные из сегмента с FIN потеряны");

        // FIN обязан быть подтверждён: ack = ISN+1 + 3 (данные) + 1 (FIN).
        let acks: Vec<u32> = drain(&mut rx).await.iter().map(|(h, _)| h.acknowledgment_number).collect();
        assert!(acks.contains(&(PEER_ISN + 5)), "FIN вместе с данными не подтверждён, ack'и: {acks:?}");
    }

    /// Точка 2: внеочередной сегмент С PSH (а PSH стоит на большинстве реальных
    /// сегментов) раньше выбрасывался целиком — вместе с ним не работал и наш
    /// дубль-ACK, так что отправитель ждал полный RTO вместо быстрого повтора.
    #[tokio::test(flavor = "multi_thread")]
    async fn out_of_order_psh_segment_is_buffered_and_dup_acked() {
        let (mut stream, mut rx) = new_stream(TcpConfig::default());
        handshake(&stream, &mut rx).await;

        // Второй сегмент приезжает первым: дыра ISN+1 .. ISN+4.
        send(&stream, ACK | PSH, PEER_ISN + 4, b"def");
        let (h, _) = next_pkt(&mut rx).await.expect("нет дубль-ACK на внеочередной сегмент");
        assert_eq!(h.acknowledgment_number, PEER_ISN + 1, "дубль-ACK должен повторять текущий ack");

        // Дыра закрыта — наверх обязаны уехать ОБА куска, по порядку.
        send(&stream, ACK | PSH, PEER_ISN + 1, b"abc");
        let mut got = Vec::new();
        while got.len() < 6 {
            let chunk = read_some(&mut stream).await;
            if chunk.is_empty() {
                break;
            }
            got.extend_from_slice(&chunk);
        }
        assert_eq!(got, b"abcdef", "внеочередной сегмент выброшен вместо пересборки");
    }

    /// Точка 4: приёмное окно обязано закрываться, когда приложение не успевает
    /// читать, и открываться обратно САМО (window update), а не ждать зонда пира.
    #[tokio::test(flavor = "multi_thread")]
    async fn receive_window_closes_when_app_stalls_and_reopens_on_read() {
        let mut config = TcpConfig::default();
        config.read_buffer_size = 8 * 1024;
        let (mut stream, mut rx) = new_stream(config);
        handshake(&stream, &mut rx).await;

        // 7 × 1200 = 8400 байт лежат в очереди наверх, приложение не читает.
        let mut seq = PEER_ISN + 1;
        for _ in 0..7 {
            send(&stream, ACK | PSH, seq, &[7u8; 1200]);
            seq += 1200;
        }
        let windows: Vec<u16> = drain(&mut rx).await.iter().map(|(h, _)| h.window_size).collect();
        assert!(windows.contains(&0), "окно так и не закрылось, объявляли: {windows:?}");

        // Приложение читает — окно должно открыться, и стек обязан сам
        // сообщить об этом: пир сидит в persist-режиме и иначе ждёт до минуты.
        let mut reopened = false;
        for _ in 0..4 {
            assert!(!read_some(&mut stream).await.is_empty(), "данные пропали");
            if drain(&mut rx).await.iter().any(|(h, _)| h.window_size > 0) {
                reopened = true;
                break;
            }
        }
        assert!(reopened, "после чтения не пришёл window update — поток встанет на секунды");
    }
}
