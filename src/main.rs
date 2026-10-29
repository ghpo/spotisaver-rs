mod download;
mod spotify;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use reqwest::Client;

#[derive(Parser)]
#[command(
    name = "spotisaver",
    about = "Backup de playlists e músicas do Spotify"
)]
struct Args {
    url: String,
    #[arg(long)]
    txt: Option<String>,
    /// Baixar o áudio das faixas para este diretório (usa yt-dlp)
    #[arg(long, value_name = "DIR")]
    download: Option<PathBuf>,
    /// Formato de áudio do download (mp3, m4a, opus...)
    #[arg(long, default_value = "mp3")]
    format: String,
    /// Downloads simultâneos
    #[arg(long, default_value_t = 3)]
    jobs: usize,
    /// Baixar apenas as N primeiras faixas (0 = todas)
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Emitir eventos de progresso em JSON, uma linha por evento
    #[arg(long)]
    json_progress: bool,
}

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}", s / 60, s % 60)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client::new();
    let pl = spotify::fetch(&client, &args.url).await?;

    if !args.json_progress {
        if pl.kind.is_track() {
            println!("Música: {}", pl.name);
        } else {
            println!("Playlist: {} ({} faixas)", pl.name, pl.tracks.len());
        }
        for (i, t) in pl.tracks.iter().enumerate() {
            println!(
                "{:>3}. {} — {}  [{}]",
                i + 1,
                t.artist,
                t.title,
                fmt_ms(t.duration_ms)
            );
        }
    }

    if let Some(path) = args.txt {
        let body: String = pl
            .tracks
            .iter()
            .map(|t| format!("{}\t{}\n", t.artist, t.title))
            .collect();
        std::fs::write(&path, format!("# {}\n{}", pl.name, body))?;
        if !args.json_progress {
            println!("TXT exportado: {}", path);
        }
    }

    let reporter = if args.json_progress {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Ok(line) = serde_json::to_string(&event) {
                    println!("{line}");
                }
            }
        });
        download::Reporter::Json(tx)
    } else {
        download::Reporter::Human
    };

    if let Some(dir) = &args.download {
        let folder = if pl.kind.is_track() {
            dir.clone()
        } else {
            let mut name = download::sanitize_filename(&pl.name);
            if name.is_empty() {
                name = "playlist".to_string();
            }
            dir.join(name)
        };

        if !args.json_progress {
            println!("Baixando para: {}", folder.display());
        }

        let summary = download::download_playlist(
            &pl,
            &folder,
            &args.format,
            args.jobs,
            args.limit,
            reporter,
        )
        .await?;

        if !summary.falhas.is_empty() && !args.json_progress {
            anyhow::bail!("{} faixa(s) falharam", summary.falhas.len());
        }
    }

    Ok(())
}
