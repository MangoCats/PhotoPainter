use std::sync::Mutex;
use std::time::{Duration, Instant};
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color, SCREEN_W, SCREEN_H};
use crate::stock_creds::API_KEY;

pub const STRIP_H: i32 = 48;
const DIVIDER_W:   i32 = 5;
const MAX_FONT:    f32 = 43.0;
const MIN_FONT:    f32 = 10.0;
const H_PAD:       i32 = 4;
const STALE_AFTER: Duration = Duration::from_secs(2 * 3600);

#[derive(Clone)]
struct Quote {
    symbol: String,
    price:  f64,
    open:   f64,
}

pub struct StockModule {
    quotes:       Mutex<Vec<Quote>>,
    tickers:      Vec<String>,
    client:       reqwest::Client,
    last_updated: Mutex<Option<Instant>>,
}

impl StockModule {
    pub fn new(tickers: Vec<String>, client: reqwest::Client) -> Self {
        Self {
            quotes:       Mutex::new(Vec::new()),
            tickers,
            client,
            last_updated: Mutex::new(None),
        }
    }

    pub async fn refresh(&self) {
        let mut results = Vec::new();
        for ticker in &self.tickers {
            match self.fetch_one(ticker).await {
                Ok(q)  => results.push(q),
                Err(e) => tracing::warn!("stock fetch failed for {ticker}: {e}"),
            }
        }
        if !results.is_empty() {
            *self.quotes.lock().unwrap() = results;
            *self.last_updated.lock().unwrap() = Some(Instant::now());
        }
    }

    async fn fetch_one(&self, ticker: &str)
        -> Result<Quote, Box<dyn std::error::Error + Send + Sync>>
    {
        let resp: serde_json::Value = self.client
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
        Ok(Quote { symbol: ticker.to_string(), price, open })
    }

    pub fn render_strip(&self, canvas: &mut E6Canvas) {
        let quotes = self.quotes.lock().unwrap().clone();
        if quotes.is_empty() { return; }

        let stale = self.last_updated.lock().unwrap()
            .map(|t| t.elapsed() > STALE_AFTER)
            .unwrap_or(true);

        let n           = quotes.len() as i32;
        let total_div_w = (n - 1) * DIVIDER_W;
        let base_sec_w  = (SCREEN_W - total_div_w) / n;
        // Strip background starts 12px lower than the region boundary; text sits 4px above screen bottom.
        let strip_y      = SCREEN_H - STRIP_H + 16;
        let strip_draw_h = STRIP_H - 16;

        // Choose font size so the widest label fits within a section
        let longest = quotes.iter()
            .map(|q| make_label(q, stale))
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
                canvas.fill_rect(x, strip_y, DIVIDER_W, strip_draw_h, E6Color::White);
                x += DIVIDER_W;
            }
            // Last section absorbs any remainder from integer division
            let sec_w = if i as i32 == n - 1 { SCREEN_W - x } else { base_sec_w };
            let bg    = if q.price >= q.open { E6Color::Green } else { E6Color::Red };
            canvas.fill_rect(x, strip_y, sec_w, strip_draw_h, bg);

            let txt     = make_label(q, stale);
            let (tw, _) = measure_text(&txt, font_size, false);
            let tx      = x + (sec_w - tw) / 2;
            draw_text(canvas, tx, text_y, &txt, font_size, E6Color::White, false);

            x += sec_w;
        }
    }
}

// Tilde prefix signals the price may not be current (market closed or refresh failed).
fn make_label(q: &Quote, stale: bool) -> String {
    if stale {
        format!("{} ~{:.2}", q.symbol, q.price)
    } else {
        format!("{} {:.2}", q.symbol, q.price)
    }
}
