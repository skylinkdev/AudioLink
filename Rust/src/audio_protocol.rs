// Android 用于音频会话协商、音量和媒体键命令的 TCP 控制端口。
// UDP 音频流由 Android 端绑定本地端口后，通过 START_UDP 控制命令告知服务端。
pub const CONTROL_PORT: u16 = 9091;

// 网络传输使用的目标采样率。
// 采集设备的实际采样率可能不同，audio_capture 会先重采样到这个采样率再发送。
// Android AudioTrack 接收端也应使用相同采样率播放。
pub const SAMPLE_RATE: u32 = 48_000;

// 网络传输使用的声道数。
// 当前固定为双声道 PCM，样本按 L/R/L/R 交错排列。
pub const CHANNELS: usize = 2;

// 每个网络音频包对应的时长，单位毫秒。
// 数值越小，理论端到端延迟越低；但每秒包数量越多，UDP 发送、调度和接收端处理压力也越高。
// 例如 5ms 在 48kHz 下等于每声道 240 个采样点，每秒约发送 200 个音频包。
pub const FRAME_MS: u32 = 10;

// PCM 负载格式：16-bit signed little-endian。
// 这个字符串会通过 mDNS TXT 记录发布给接收端。
pub const FORMAT: &str = "s16le";

// 当前应用层协议标识。
// 这个字符串同样会通过 mDNS 发布，用于让接收端确认服务类型和解析方式。
pub const PROTOCOL: &str = "tcp-control-udp-pcm-fec";

// UDP 音频包头格式：

// 意思是每个合法音频 UDP 包开头 4 个字节都必须是： LAP1
pub const UDP_PACKET_MAGIC: [u8; 4] = *b"LAP1";

// UDP 包协议版本。当前只保留这一种协议格式，不做旧格式兼容。
pub const UDP_PACKET_VERSION: u8 = 1;

// UDP 包头固定长度。当前格式包含主 PCM 长度和 FEC 元数据，所以是 48 字节。
pub const UDP_PACKET_HEADER_BYTES: usize = 48;

// 表示当前包没有携带 FEC 冗余数据。
pub const UDP_PACKET_FEC_CODEC_NONE: u8 = 0;

// FEC 冗余数据编码：完整 stereo s16le PCM，用于在丢失上一包时原音质补帧。
pub const UDP_PACKET_FEC_CODEC_PCM_S16LE_STEREO: u8 = 1;

// 是否启用前向纠错。启用后，每个包会附带上一包的完整 PCM 冗余数据。
pub const UDP_PACKET_FEC_ENABLED: bool = true;

// 将每个 UDP 音频数据包重复发送指定的次数。保持序列号和时间戳不变，
// 这样接收端就能丢弃重复包，避免同一段音频被播放两次。
// 默认值为 1，表示不重复发送。增大这个值可以提高丢包情况下的音频连续性，但也会增加网络带宽占用。
pub const UDP_PACKET_SEND_MULTIPLIER: usize = 1;

// 单个音频包“每个声道”的最大采样点数。
// 计算公式是 SAMPLE_RATE * FRAME_MS / 1000，并使用向上取整。
// 当前 48_000Hz * 5ms 可以整除，结果为 240。
// 保留向上取整是为了以后修改采样率或帧长时，即使不能整除，也能预留足够的包缓冲容量。
pub const MAX_FRAME_SAMPLES_PER_CHANNEL: usize =
    ((SAMPLE_RATE as usize * FRAME_MS as usize) + 999) / 1000;

// 单个音频包中所有声道交错后的最大采样点数。
// 双声道时等于“每声道采样点数 * 2”。
pub const MAX_FRAME_SAMPLES: usize = MAX_FRAME_SAMPLES_PER_CHANNEL * CHANNELS;

// 单个音频包的最大字节数。
// 当前格式是 i16 PCM，每个采样点占 2 字节，所以这里用 i16 的大小计算。
pub const MAX_FRAME_BYTES: usize = MAX_FRAME_SAMPLES * std::mem::size_of::<i16>();
