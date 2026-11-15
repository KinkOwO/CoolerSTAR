//! 视频背景：ffmpeg 解成 320×240 RGB24。
//!
//! 循环要"无缝"，关键不是 `-stream_loop`——那只是把文件首尾硬接，
//! 源文件最后一帧和第一帧差多少就跳多少。这里先把整段视频解到内存，
//! 再把尾巴那 K 帧和开头 K 帧做交叉淡化，得到一个天然首尾相接的环，
//! 之后按时间索引取帧，连 ffmpeg 都不用一直跑。
//!
//! K 不是拍脑袋定的：解码完会把 K=3..16 都试一遍，挑接缝帧差最小的那个。

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 别让 ffmpeg 顶出一个控制台窗口。
///
/// 我们自己编成 GUI 子系统（`windows_subsystem = "windows"`），所以启动
/// console 程序时系统会给它新开一个控制台 —— 冷启动那一下闪出来的黑框就是它。
/// `CREATE_NO_WINDOW` 让它用不显示的控制台跑，功能完全一样。
#[cfg(windows)]
fn quiet(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x08000000) // CREATE_NO_WINDOW
}
#[cfg(not(windows))]
fn quiet(cmd: &mut Command) -> &mut Command {
    cmd
}

pub const W: usize = 320;
pub const H: usize = 240;
const FRAME: usize = W * H * 3;
/// 解码帧率（也是播放帧率）
const FPS: usize = 15;
/// 内存上限：超过这么多帧就退回流式解码（不做淡化）
const MAX_FRAMES: usize = 420;

pub struct VideoSource {
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    pub error: Arc<Mutex<String>>,
    pub ffmpeg: String,
    pub alive: Arc<AtomicBool>,
    child: Option<Child>,
    /// 预载模式：整段视频已经在内存里，按时间取帧
    frames: Option<Arc<Loop>>,
    /// 这一路视频的循环方式（给界面看）
    pub note: String,
}

struct Loop {
    buf: Vec<u8>,
    len: usize,
    t0: Instant,
}

impl Drop for VideoSource {
    fn drop(&mut self) {
        self.stop();
    }
}

impl VideoSource {
    pub fn start(path: &str, ffmpeg: &str) -> Self {
        let error = Arc::new(Mutex::new(String::new()));
        let alive = Arc::new(AtomicBool::new(true));

        let (frames, note, stream) = match decode_all(ffmpeg, path) {
            Decoded::Full(raw, n) => match build_loop(&raw, n) {
                Some((buf, len, k)) => {
                    let note = format!("首尾交叉淡化 {} 帧（{:.2}s）", k, k as f32 / FPS as f32);
                    (
                        Some(Arc::new(Loop {
                            buf,
                            len,
                            t0: Instant::now(),
                        })),
                        note,
                        None,
                    )
                }
                None => (None, String::new(), Some(path.to_string())),
            },
            Decoded::TooLong => (
                None,
                format!("视频较长，直接流式循环（不做淡化）"),
                Some(path.to_string()),
            ),
            Decoded::Fail(msg) => {
                if let Ok(mut e) = error.lock() {
                    if !msg.is_empty() {
                        *e = msg;
                    }
                }
                (None, String::new(), Some(path.to_string()))
            }
        };

        let latest = Arc::new(Mutex::new(None::<Vec<u8>>));
        let stop = Arc::new(AtomicBool::new(false));

        // 预载成功就不需要 ffmpeg 常驻了
        let child = stream.map(|p| Self::spawn_stream(&p, ffmpeg, &latest, &stop, &error, &alive));

        Self {
            latest,
            stop,
            error,
            ffmpeg: ffmpeg.to_string(),
            alive,
            child: child.flatten(),
            frames,
            note,
        }
    }

    fn spawn_stream(
        path: &str,
        ffmpeg: &str,
        latest: &Arc<Mutex<Option<Vec<u8>>>>,
        stop: &Arc<AtomicBool>,
        error: &Arc<Mutex<String>>,
        alive: &Arc<AtomicBool>,
    ) -> Option<Child> {
        let mut cmd = Command::new(ffmpeg);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-stream_loop",
            "-1",
            "-i",
            path,
            "-an",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            "320x240",
            "-r",
            "15",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
        let mut child = quiet(&mut cmd).spawn().ok()?;

        let stdout = child.stdout.take();
        let latest2 = latest.clone();
        let stop2 = stop.clone();
        let error2 = error.clone();
        let alive2 = alive.clone();
        std::thread::spawn(move || {
            let Some(mut out) = stdout else {
                alive2.store(false, Ordering::SeqCst);
                return;
            };
            let mut buf = vec![0u8; FRAME];
            while !stop2.load(Ordering::Relaxed) {
                match out.read_exact(&mut buf) {
                    Ok(()) => {
                        if let Ok(mut g) = latest2.lock() {
                            *g = Some(buf.clone());
                        }
                    }
                    Err(e) => {
                        if let Ok(mut m) = error2.lock() {
                            if m.is_empty() {
                                *m = format!("视频流中断：{e}");
                            }
                        }
                        break;
                    }
                }
            }
            alive2.store(false, Ordering::SeqCst);
        });
        Some(child)
    }

    /// 当前该显示的帧（裸 RGB24，不拷成 RGBA）
    pub fn raw_peek(&self) -> Option<Vec<u8>> {
        self.peek()
    }

    /// 当前该显示的帧
    pub fn peek(&self) -> Option<Vec<u8>> {
        if let Some(l) = &self.frames {
            if l.len == 0 {
                return None;
            }
            let elapsed = l.t0.elapsed().as_secs_f64();
            let idx = ((elapsed * FPS as f64) as usize) % l.len;
            let s = idx * FRAME;
            return Some(l.buf[s..s + FRAME].to_vec());
        }
        self.latest.lock().ok().and_then(|g| g.clone())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            std::thread::sleep(Duration::from_millis(60));
        }
    }
}

enum Decoded {
    Full(Vec<u8>, usize),
    TooLong,
    Fail(String),
}

/// 把整段视频解成连续的 320×240 RGB24 帧
fn decode_all(ffmpeg: &str, path: &str) -> Decoded {
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        path,
        "-an",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
        "320x240",
        "-r",
        "15",
        "-",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .stdin(Stdio::null());
    let mut child = match quiet(&mut cmd).spawn() {
        Ok(c) => c,
        Err(e) => return Decoded::Fail(format!("ffmpeg 启动失败：{e}")),
    };

    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return Decoded::Fail("拿不到 ffmpeg 输出".into()),
    };

    let mut buf: Vec<u8> = Vec::with_capacity(FRAME * 64);
    let mut one = vec![0u8; FRAME];
    let mut count = 0usize;
    let mut too_long = false;
    loop {
        match stdout.read_exact(&mut one) {
            Ok(()) => {
                buf.extend_from_slice(&one);
                count += 1;
                if count > MAX_FRAMES {
                    too_long = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    drop(stdout);

    let mut err_text = String::new();
    if let Some(mut se) = child.stderr.take() {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        err_text = s.trim().to_string();
    }

    if too_long {
        let _ = child.kill();
        let _ = child.wait();
        return Decoded::TooLong;
    }
    let _ = child.wait();

    if count < 4 {
        return Decoded::Fail(if err_text.is_empty() {
            "解不出帧，可能是不支持的编码".to_string()
        } else {
            err_text.chars().take(200).collect()
        });
    }
    Decoded::Full(buf, count)
}

/// 交叉淡化接缝：返回 (新帧缓冲, 帧数, 用了几帧淡化)
fn build_loop(raw: &[u8], n: usize) -> Option<(Vec<u8>, usize, usize)> {
    if n < 12 {
        return None;
    }
    let f = |i: usize| -> &[u8] { &raw[i * FRAME..i * FRAME + FRAME] };

    let max_k = (n / 4).min(16);
    let mut best: Option<(usize, f64)> = None;
    for k in 3..=max_k {
        // 这次淡化之后，第一帧是 blend(0)，最后一帧是 F[n-k-1]
        let first = blend(f(n - k), f(0), 1.0 / (k + 1) as f32);
        let wrap = mad(&first, f(n - k - 1));
        if best.map_or(true, |(_, b)| wrap < b - 1e-6) {
            best = Some((k, wrap));
        }
    }
    let (k, _) = best?;

    let mut buf = Vec::with_capacity((n - k) * FRAME);
    for i in 0..k {
        let t = (i + 1) as f32 / (k + 1) as f32;
        buf.extend_from_slice(&blend(f(n - k + i), f(i), t));
    }
    for i in k..n - k {
        buf.extend_from_slice(f(i));
    }
    // k 帧淡化 + (n-2k) 帧原样 = n-k 帧
    Some((buf, n - k, k))
}

fn blend(a: &[u8], b: &[u8], t: f32) -> Vec<u8> {
    let t = t.clamp(0.0, 1.0);
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as f32 * (1.0 - t) + y as f32 * t) as u8)
        .collect()
}

fn mad(a: &[u8], b: &[u8]) -> f64 {
    let mut s = 0u64;
    for i in 0..a.len() {
        s += a[i].abs_diff(b[i]) as u64;
    }
    s as f64 / a.len() as f64
}
