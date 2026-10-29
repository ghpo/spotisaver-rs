use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

use crate::spotify::{Playlist, Track};

#[derive(Debug, Default)]
pub struct DownloadSummary {
    pub baixadas: usize,
    pub puladas: usize,
    pub falhas: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Start {
        name: String,
        total: usize,
        single: bool,
    },
    Track {
        index: usize,
        artist: String,
        title: String,
        status: TrackStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Progress {
        percent: f64,
    },
    Done {
        downloaded: usize,
        skipped: usize,
        failed: usize,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackStatus {
    Downloading,
    Done,
    Skipped,
    Failed,
}

#[derive(Clone)]
pub enum Reporter {
    Human,
    Json(mpsc::UnboundedSender<Event>),
}

impl Reporter {
    fn emit(&self, event: Event) {
        match self {
            Reporter::Human => match &event {
                Event::Track {
                    index,
                    artist,
                    title,
                    status,
                    error,
                } => match status {
                    TrackStatus::Downloading => println!("[{index}] baixando: {artist} — {title}"),
                    TrackStatus::Done => println!("[{index}] ok: {title}"),
                    TrackStatus::Skipped => println!("[{index}] já existe, pulando: {title}"),
                    TrackStatus::Failed => println!(
                        "[{index}] falhou: {title} — {}",
                        error.as_deref().unwrap_or("erro desconhecido")
                    ),
                },
                Event::Done {
                    downloaded,
                    skipped,
                    failed,
                } => println!("Resumo: {downloaded} baixadas, {skipped} puladas, {failed} falhas"),
                Event::Start { .. } | Event::Progress { .. } => {}
            },
            Reporter::Json(tx) => {
                let _ = tx.send(event);
            }
        }
    }
}

pub fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .trim()
        .to_string()
}

fn safe_or(s: &str, fallback: &str) -> String {
    let s = sanitize_filename(s);
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

pub fn track_basename(index: usize, track: &Track) -> String {
    format!(
        "{:03} - {} - {}",
        index,
        safe_or(&track.artist, "desconhecido"),
        safe_or(&track.title, "faixa")
    )
}

pub fn track_filename(index: usize, track: &Track, ext: &str) -> String {
    format!("{}.{}", track_basename(index, track), ext)
}

fn search_query(track: &Track) -> String {
    if track.artist.trim().is_empty() {
        format!("{} audio", track.title)
    } else {
        format!("{} {} audio", track.artist, track.title)
    }
}

fn parse_progress_line(line: &str) -> Option<f64> {
    let raw = line.strip_prefix("PROGRESS ")?;
    let value = raw.trim().trim_end_matches('%').trim();
    let percent = value.parse::<f64>().ok()?;
    Some((percent / 100.0).clamp(0.0, 1.0))
}

async fn wait_for_cancel() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
    }

    #[cfg(not(unix))]
    std::future::pending::<()>().await;
}

async fn download_one(
    track: &Track,
    stem: &Path,
    format: &str,
    index: usize,
    progress: mpsc::UnboundedSender<(usize, f64)>,
) -> Result<()> {
    let template = format!("{}.%(ext)s", stem.display());
    let mut child = Command::new("yt-dlp")
        .arg("-x")
        .arg("--audio-format")
        .arg(format)
        .arg("--no-playlist")
        .arg("--newline")
        .arg("--progress")
        .arg("--progress-template")
        .arg("download:PROGRESS %(progress._percent_str)s")
        .arg("--no-warnings")
        .arg("-R")
        .arg("2")
        .arg("-o")
        .arg(template)
        .arg(format!("ytsearch1:{}", search_query(track)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("falha ao executar yt-dlp (está instalado?)")?;

    let stdout = child
        .stdout
        .take()
        .context("stdout do yt-dlp indisponível")?;
    let stderr = child
        .stderr
        .take()
        .context("stderr do yt-dlp indisponível")?;

    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        let mut stderr = stderr;
        let _ = stderr.read_to_string(&mut buf).await;
        buf
    });

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(fraction) = parse_progress_line(&line) {
            let _ = progress.send((index, fraction));
        }
    }

    let status = child.wait().await.context("yt-dlp não terminou")?;
    let stderr = stderr_task.await.unwrap_or_default();

    if !status.success() {
        let msg = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("erro desconhecido do yt-dlp")
            .trim()
            .to_string();
        return Err(anyhow!(msg));
    }

    let _ = progress.send((index, 1.0));
    Ok(())
}

pub async fn download_playlist(
    playlist: &Playlist,
    dir: &Path,
    format: &str,
    jobs: usize,
    limit: usize,
    reporter: Reporter,
) -> Result<DownloadSummary> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("não consegui criar a pasta {}", dir.display()))?;

    let total = if limit == 0 {
        playlist.tracks.len()
    } else {
        limit.min(playlist.tracks.len())
    };

    reporter.emit(Event::Start {
        name: playlist.name.clone(),
        total,
        single: playlist.kind.is_track(),
    });

    let sem = Arc::new(Semaphore::new(jobs.max(1)));
    let (frac_tx, mut frac_rx) = mpsc::unbounded_channel::<(usize, f64)>();
    let mut set: JoinSet<(usize, String, Result<()>)> = JoinSet::new();
    let mut summary = DownloadSummary::default();
    let mut fractions: HashMap<usize, f64> = HashMap::new();
    let mut last_percent = -1.0f64;
    let mut cancel = Box::pin(wait_for_cancel());

    for (i, track) in playlist.tracks.iter().take(total).enumerate() {
        let index = i + 1;
        let final_path = dir.join(track_filename(index, track, format));
        if final_path.exists() {
            summary.puladas += 1;
            fractions.insert(index, 1.0);
            reporter.emit(Event::Track {
                index,
                artist: track.artist.clone(),
                title: track.title.clone(),
                status: TrackStatus::Skipped,
                error: None,
            });
            continue;
        }

        let permit = sem.clone();
        let dir = dir.to_path_buf();
        let format = format.to_string();
        let track = track.clone();
        let reporter = reporter.clone();
        let progress = frac_tx.clone();

        set.spawn(async move {
            let permit = permit.acquire_owned().await.expect("semáforo fechado");
            reporter.emit(Event::Track {
                index,
                artist: track.artist.clone(),
                title: track.title.clone(),
                status: TrackStatus::Downloading,
                error: None,
            });
            let stem = dir.join(track_basename(index, &track));
            let result = download_one(&track, &stem, &format, index, progress).await;
            drop(permit);
            (index, track.title.clone(), result)
        });
    }

    while !set.is_empty() {
        tokio::select! {
            joined = set.join_next() => {
                match joined {
                    Some(Ok((index, title, Ok(())))) => {
                        summary.baixadas += 1;
                        fractions.insert(index, 1.0);
                        reporter.emit(Event::Track {
                            index,
                            artist: playlist.tracks.get(index - 1).map(|t| t.artist.clone()).unwrap_or_default(),
                            title,
                            status: TrackStatus::Done,
                            error: None,
                        });
                    }
                    Some(Ok((index, title, Err(e)))) => {
                        fractions.insert(index, 1.0);
                        summary.falhas.push(format!("{index}: {title} ({e})"));
                        reporter.emit(Event::Track {
                            index,
                            artist: playlist.tracks.get(index - 1).map(|t| t.artist.clone()).unwrap_or_default(),
                            title,
                            status: TrackStatus::Failed,
                            error: Some(e.to_string()),
                        });
                    }
                    Some(Err(e)) => summary.falhas.push(format!("tarefa abortada: {e}")),
                    None => break,
                }
            }
            Some((index, fraction)) = frac_rx.recv() => {
                let entry = fractions.entry(index).or_insert(0.0);
                if fraction > *entry {
                    *entry = fraction;
                }
            }
            _ = &mut cancel => {
                set.abort_all();
                return Err(anyhow!("download cancelado"));
            }
        }

        let done: f64 = fractions.values().copied().sum();
        let percent = if total == 0 {
            100.0
        } else {
            (done / total as f64 * 100.0).min(100.0)
        };
        if percent - last_percent >= 0.5 {
            last_percent = percent;
            reporter.emit(Event::Progress { percent });
        }
    }

    reporter.emit(Event::Progress { percent: 100.0 });
    reporter.emit(Event::Done {
        downloaded: summary.baixadas,
        skipped: summary.puladas,
        failed: summary.falhas.len(),
    });

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(artist: &str, title: &str) -> Track {
        Track {
            spotify_id: "x".into(),
            title: title.into(),
            artist: artist.into(),
            duration_ms: 0,
        }
    }

    #[test]
    fn sanitiza_caracteres_invalidos() {
        assert_eq!(sanitize_filename("AC/DC: Live?"), "AC_DC_ Live_");
        assert_eq!(sanitize_filename("  Nome ok  "), "Nome ok");
    }

    #[test]
    fn nome_de_arquivo_com_indice() {
        assert_eq!(
            track_filename(1, &track("AC/DC", "T.N.T."), "mp3"),
            "001 - AC_DC - T.N.T.mp3"
        );
    }

    #[test]
    fn campos_vazios_tem_fallback() {
        assert_eq!(
            track_filename(7, &track("", ""), "opus"),
            "007 - desconhecido - faixa.opus"
        );
    }

    #[test]
    fn le_porcentagem_do_yt_dlp() {
        assert_eq!(parse_progress_line("PROGRESS  42.3%"), Some(0.423));
        assert_eq!(parse_progress_line("PROGRESS 100%"), Some(1.0));
        assert_eq!(parse_progress_line("PROGRESS N/A"), None);
        assert_eq!(parse_progress_line("outra linha"), None);
    }

    #[test]
    fn evento_json_tem_formato_estavel() {
        let event = Event::Progress { percent: 12.5 };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"event":"progress","percent":12.5}"#
        );
    }
}
