use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Track {
    #[allow(dead_code)]
    pub spotify_id: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotifyKind {
    Playlist,
    Track,
}

impl SpotifyKind {
    pub fn is_track(self) -> bool {
        matches!(self, SpotifyKind::Track)
    }
}

#[derive(Debug)]
pub struct Playlist {
    #[allow(dead_code)]
    pub id: String,
    pub name: String,
    pub tracks: Vec<Track>,
    pub kind: SpotifyKind,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbedTrack {
    uri: String,
    title: String,
    #[serde(default)]
    subtitle: String,
    #[serde(default)]
    duration: u64,
}

pub fn parse_spotify_url(url: &str) -> Result<(SpotifyKind, String)> {
    const PATTERNS: &[(&str, SpotifyKind)] = &[
        ("spotify:playlist:", SpotifyKind::Playlist),
        ("spotify:track:", SpotifyKind::Track),
        ("playlist/", SpotifyKind::Playlist),
        ("track/", SpotifyKind::Track),
    ];

    for (pat, kind) in PATTERNS {
        if let Some(pos) = url.find(pat) {
            let rest = &url[pos + pat.len()..];
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !id.is_empty() {
                return Ok((*kind, id));
            }
        }
    }

    Err(anyhow!(
        "não encontrei um link de playlist ou música em: {url}"
    ))
}

fn json_span(s: &str, start: usize) -> Result<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, &c) in bytes.iter().enumerate().skip(start) {
        if esc {
            esc = false;
            continue;
        }
        match c {
            b'"' => in_str = !in_str,
            b'\\' if in_str => esc = true,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i + 1);
                }
            }
            _ => {}
        }
    }
    Err(anyhow!("JSON do embed não terminou corretamente"))
}

fn artist_from_entity(entity: &serde_json::Value) -> String {
    entity["artists"]
        .as_array()
        .map(|artists| {
            artists
                .iter()
                .filter_map(|a| a["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn track_from_entity(entity: &serde_json::Value) -> Option<Track> {
    let title = entity["title"]
        .as_str()
        .or_else(|| entity["name"].as_str())?
        .to_string();
    let spotify_id = entity["id"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            entity["uri"]
                .as_str()
                .and_then(|uri| uri.strip_prefix("spotify:track:").map(str::to_string))
        })
        .unwrap_or_default();

    Some(Track {
        spotify_id,
        title,
        artist: artist_from_entity(entity),
        duration_ms: entity["duration"].as_u64().unwrap_or(0),
    })
}

pub fn parse_embed_html(kind: SpotifyKind, id: &str, html: &str) -> Result<Playlist> {
    let pos = html
        .find("__NEXT_DATA__")
        .ok_or_else(|| anyhow!("playlist ou música não encontrada (sem dados no embed)"))?;
    let gt = pos
        + html[pos..]
            .find('>')
            .ok_or_else(|| anyhow!("JSON inicial ausente"))?;
    let start = gt
        + 1
        + html[gt + 1..]
            .find('{')
            .ok_or_else(|| anyhow!("JSON inicial ausente"))?;
    let end = json_span(html, start)?;

    let value: serde_json::Value = serde_json::from_str(&html[start..end])
        .context("não consegui parsear os metadados do Spotify")?;

    let data = &value["props"]["pageProps"]["state"]["data"];
    let entity = &data["entity"];
    if entity.is_null() {
        return Err(anyhow!(
            "playlist ou música não encontrada (talvez seja privada)"
        ));
    }

    let name = entity["title"]
        .as_str()
        .or_else(|| entity["name"].as_str())
        .unwrap_or("spotify")
        .to_string();

    let tracks = match kind {
        SpotifyKind::Track => track_from_entity(entity).into_iter().collect(),
        SpotifyKind::Playlist => data["entity"]["trackList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value::<EmbedTrack>(v).ok())
            .filter_map(|t| {
                let sid = t.uri.strip_prefix("spotify:track:")?;
                Some(Track {
                    spotify_id: sid.to_string(),
                    title: t.title,
                    artist: t.subtitle,
                    duration_ms: t.duration,
                })
            })
            .collect(),
    };

    Ok(Playlist {
        id: id.to_string(),
        name,
        tracks,
        kind,
    })
}

pub async fn fetch(client: &Client, url: &str) -> Result<Playlist> {
    let (kind, id) = parse_spotify_url(url)?;
    let endpoint = match kind {
        SpotifyKind::Playlist => "playlist",
        SpotifyKind::Track => "track",
    };
    let html = client
        .get(format!("https://open.spotify.com/embed/{endpoint}/{id}"))
        .header("User-Agent", "Mozilla/5.0 spotisaver-rs")
        .send()
        .await
        .context("falha ao buscar a página do embed do Spotify")?
        .text()
        .await?;
    parse_embed_html(kind, &id, &html)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<html><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"state":{"data":{"entity":{"type":"playlist","name":"Teste \"com aspas\"","trackList":[{"uri":"spotify:track:AAA","title":"Faixa 1"},{"uri":"spotify:track:BBB","title":"Faixa 2","subtitle":"Artista","duration":4200}]}}}}}}</script></html>"#;

    const TRACK_FIXTURE: &str = r#"<html><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"state":{"data":{"entity":{"type":"track","name":"Never Gonna Give You Up","title":"Never Gonna Give You Up","uri":"spotify:track:4uLU6hMCjMI75M1A2tKUQC","id":"4uLU6hMCjMI75M1A2tKUQC","artists":[{"name":"Rick Astley","uri":"spotify:artist:0gxyHStUsqpMadRV0Di1Qt"}],"duration":213573}}}}}}</script></html>"#;

    #[test]
    fn extrai_url_de_playlist() {
        assert_eq!(
            parse_spotify_url("https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=abc")
                .unwrap(),
            (SpotifyKind::Playlist, "37i9dQZF1DXcBWIGoYBM5M".to_string())
        );
        assert_eq!(
            parse_spotify_url("spotify:playlist:12abc").unwrap(),
            (SpotifyKind::Playlist, "12abc".to_string())
        );
        assert!(parse_spotify_url("https://open.spotify.com/search?q=rock").is_err());
    }

    #[test]
    fn extrai_url_de_musica() {
        assert_eq!(
            parse_spotify_url(
                "https://open.spotify.com/intl-pt/track/4uLU6hMCjMI75M1A2tKUQC?si=xyz"
            )
            .unwrap(),
            (SpotifyKind::Track, "4uLU6hMCjMI75M1A2tKUQC".to_string())
        );
        assert_eq!(
            parse_spotify_url("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap(),
            (SpotifyKind::Track, "4uLU6hMCjMI75M1A2tKUQC".to_string())
        );
    }

    #[test]
    fn parse_fixture_de_playlist() {
        let pl = parse_embed_html(SpotifyKind::Playlist, "xyz", FIXTURE).unwrap();
        assert_eq!(pl.name, "Teste \"com aspas\"");
        assert_eq!(pl.tracks.len(), 2);
        assert_eq!(pl.tracks[0].spotify_id, "AAA");
        assert_eq!(pl.tracks[1].artist, "Artista");
        assert_eq!(pl.tracks[1].duration_ms, 4200);
    }

    #[test]
    fn parse_fixture_de_musica() {
        let pl =
            parse_embed_html(SpotifyKind::Track, "4uLU6hMCjMI75M1A2tKUQC", TRACK_FIXTURE).unwrap();
        assert_eq!(pl.name, "Never Gonna Give You Up");
        assert_eq!(pl.tracks.len(), 1);
        assert_eq!(pl.tracks[0].artist, "Rick Astley");
        assert_eq!(pl.tracks[0].duration_ms, 213573);
        assert!(pl.kind.is_track());
    }

    #[test]
    fn entity_null_erra() {
        let html = "<html><script id=\"__NEXT_DATA__\" type=\"application/json\">{\"props\":{\"pageProps\":{\"state\":{\"data\":{\"entity\":null}}}}}</script></html>";
        assert!(parse_embed_html(SpotifyKind::Playlist, "xyz", html).is_err());
    }

    #[test]
    fn sem_next_data_erra() {
        assert!(parse_embed_html(
            SpotifyKind::Playlist,
            "xyz",
            "<html><body>404</body></html>"
        )
        .is_err());
    }
}
