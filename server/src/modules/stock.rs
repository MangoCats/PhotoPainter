use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color, SCREEN_W, SCREEN_H};
use crate::stock_creds::API_KEY;

/// Height of the strip along the bottom edge of the screen.
pub const STRIP_H: i32 = 32;
const DIVIDER_W:   i32 = 5;
const MAX_FONT:    f32 = 43.0;
const MIN_FONT:    f32 = 10.0;
const H_PAD:       i32 = 4;
/// A quote older than this is shown with a `~` prefix.
const STALE_AFTER: Duration = Duration::from_secs(2 * 3600);

#[derive(Clone)]
struct Quote {
    symbol: String,
    price:  f64,
    open:   f64,
    at:     Instant,   // when this quote was fetched
}

pub struct StockModule {
    quotes:  Mutex<Vec<Quote>>,   // in ticker-file order
    tickers: Vec<String>,
    client:  reqwest::Client,
}

impl StockModule {
    pub fn new(tickers: Vec<String>, client: reqwest::Client) -> Self {
        Self { quotes: Mutex::new(Vec::new()), tickers, client }
    }

    /// Fetch every ticker concurrently.  A ticker whose fetch fails keeps its previous quote
    /// (which then ages toward the `~` marker) instead of disappearing from the strip.
    pub async fn refresh(&self) {
        let mut set = tokio::task::JoinSet::new();
        for ticker in &self.tickers {
            let client = self.client.clone();
            let ticker = ticker.clone();
            set.spawn(async move {
                let result = fetch_one(&client, &ticker).await.map_err(|e| e.to_string());
                (ticker, result)
            });
        }

        let mut fresh: HashMap<String, Quote> = HashMap::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((ticker, Ok(q))) => { fresh.insert(ticker, q); }
                Ok((ticker, Err(e))) => tracing::warn!("stock fetch failed for {ticker}: {e}"),
                Err(e) => tracing::warn!("stock fetch task failed: {e}"),
            }
        }

        let mut quotes = self.quotes.lock().unwrap();
        let merged: Vec<Quote> = self.tickers.iter()
            .filter_map(|t| fresh.remove(t).or_else(|| quotes.iter().find(|q| &q.symbol == t).cloned()))
            .collect();
        *quotes = merged;
    }

    pub fn render_strip(&self, canvas: &mut E6Canvas) {
        let quotes = self.quotes.lock().unwrap().clone();
        if quotes.is_empty() { return; }

        let n           = quotes.len() as i32;
        let total_div_w = (n - 1) * DIVIDER_W;
        let base_sec_w  = (SCREEN_W - total_div_w) / n;
        let strip_y     = SCREEN_H - STRIP_H;

        // Choose font size so the widest label fits within a section
        let longest = quotes.iter()
            .map(make_label)
            .max_by_key(|s| measure_text(s, MAX_FONT, false).0)
            .unwrap_or_default();

        let mut font_size = MAX_FONT;
        while font_size > MIN_FONT {
            let (w, _) = measure_text(&longest, font_size, false);
            if w <= base_sec_w - H_PAD * 2 { break; }
            font_size -= 0.5;
        }

        let ascent = measure_text("A", font_size, false).1;
        let text_y = SCREEN_H - ascent - 4;

        let mut x = 0i32;
        for (i, q) in quotes.iter().enumerate() {
            if i > 0 {
                canvas.fill_rect(x, strip_y, DIVIDER_W, STRIP_H, E6Color::White);
                x += DIVIDER_W;
            }
            // Last section absorbs any remainder from integer division
            let sec_w = if i as i32 == n - 1 { SCREEN_W - x } else { base_sec_w };
            let bg    = if q.price >= q.open { E6Color::Green } else { E6Color::Red };
            canvas.fill_rect(x, strip_y, sec_w, STRIP_H, bg);

            let txt     = make_label(q);
            let (tw, _) = measure_text(&txt, font_size, false);
            let tx      = x + (sec_w - tw) / 2;
            draw_text(canvas, tx, text_y, &txt, font_size, E6Color::White, false);

            x += sec_w;
        }
    }
}

async fn fetch_one(client: &reqwest::Client, ticker: &str)
    -> Result<Quote, Box<dyn std::error::Error + Send + Sync>>
{
    let resp: serde_json::Value = client
        .get("https://finnhub.io/api/v1/quote")
        .query(&[("symbol", ticker), ("token", API_KEY)])
        .send().await?.json().await?;

    let current = resp["c"].as_f64().unwrap_or(0.0);
    let prev_close = resp["pc"].as_f64().unwrap_or(0.0);
    // Use last trade price; fall back to previous close when market is closed (c = 0)
    let price = if current > 0.0 { current } else { prev_close };
    let open_raw = resp["o"].as_f64().unwrap_or(0.0);
    // When market hasn't opened yet (o = 0), compare against previous close so display is flat
    let open  = if open_raw > 0.0 { open_raw } else { price };

    if price == 0.0 {
        return Err(format!("no price data for {ticker}").into());
    }
    Ok(Quote { symbol: ticker.to_string(), price, open, at: Instant::now() })
}

// A `~` prefix marks a quote that is more than STALE_AFTER old (its last refresh failed).
fn make_label(q: &Quote) -> String {
    if q.at.elapsed() > STALE_AFTER {
        format!("{} ~{:.2}", q.symbol, q.price)
    } else {
        format!("{} {:.2}", q.symbol, q.price)
    }
}
