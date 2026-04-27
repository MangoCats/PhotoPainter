use std::sync::Mutex;
use std::time::{Duration, Instant};
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use crate::teller_creds::{ACCESS_TOKEN, ACCOUNT_ID, CERT_PATH, KEY_PATH};
use super::{Module, Rect};
use super::rain;

const SIZE_PX:     f32 = 28.0;
const TXN_SIZE_PX: f32 = 17.0;
const MARGIN:      i32 = 8;
const LINE_GAP:    i32 = 4;
const TXN_GAP:     i32 = 3;
const Y_START:     i32 = rain::GCAL_Y_START;
const MAX_TXN:     usize = 5;
const STALE_AFTER: Duration = Duration::from_secs(3600);

// Total lines rendered: 1 balance + up to MAX_TXN transactions.
// Called by renderer to compute where gcal sits below the bank block.
pub fn display_height() -> i32 {
    // Balance line overlaps into the rain area above, so only transaction lines
    // consume calendar space.
    let txn_h = measure_text("A", TXN_SIZE_PX, false).1 + TXN_GAP;
    txn_h * MAX_TXN as i32
}

#[derive(Clone)]
struct Transaction {
    amount:  f64,    // Teller: negative = debit (out), positive = credit (in)
    name:    String,
    date:    String, // "MM/DD"
    pending: bool,
}

#[derive(Clone)]
struct BankData {
    balance:      f64,
    transactions: Vec<Transaction>,
}

pub struct BankModule {
    data:    Mutex<Option<BankData>>,
    client:  reqwest::Client,
    last_ok: Mutex<Option<Instant>>,
}

impl BankModule {
    pub fn new() -> Self {
        Self {
            data:    Mutex::new(None),
            client:  Self::build_client(),
            last_ok: Mutex::new(None),
        }
    }

    fn build_client() -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .user_agent("PhotoPainter/1.0 (github.com/photopainter)")
            .timeout(Duration::from_secs(15));

        match (std::fs::read(CERT_PATH), std::fs::read(KEY_PATH)) {
            (Ok(cert), Ok(key)) => {
                let mut pem = cert;
                pem.extend_from_slice(&key);
                match reqwest::Identity::from_pem(&pem) {
                    Ok(id) => { builder = builder.identity(id); }
                    Err(e) => tracing::warn!("teller: could not parse mTLS identity: {e}"),
                }
            }
            (Err(e), _) => tracing::warn!("teller: could not read {CERT_PATH}: {e}"),
            (_, Err(e)) => tracing::warn!("teller: could not read {KEY_PATH}: {e}"),
        }

        builder.build().expect("failed to build teller HTTP client")
    }

    pub async fn refresh(&self) {
        match self.fetch().await {
            Ok(data) => {
                *self.data.lock().unwrap()    = Some(data);
                *self.last_ok.lock().unwrap() = Some(Instant::now());
            }
            Err(e) => tracing::warn!("bank fetch failed: {e}"),
        }
    }

    async fn fetch(&self) -> Result<BankData, Box<dyn std::error::Error + Send + Sync>> {
        let base = format!("https://api.teller.io/accounts/{ACCOUNT_ID}");

        // Balance — amounts returned as strings
        let bal: serde_json::Value = self.client
            .get(format!("{base}/balances"))
            .basic_auth(ACCESS_TOKEN, Some(""))
            .send().await?.json().await?;

        let balance = bal["available"].as_str()
            .or_else(|| bal["ledger"].as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or("missing balance")?;

        // Transactions — array returned directly, newest first
        let txns: serde_json::Value = self.client
            .get(format!("{base}/transactions"))
            .basic_auth(ACCESS_TOKEN, Some(""))
            .send().await?.json().await?;

        let transactions = txns.as_array()
            .map(|arr| {
                arr.iter().take(MAX_TXN).filter_map(|t| {
                    // Teller returns amounts as decimal strings; negative = debit (money out)
                    let amount  = t["amount"].as_str()
                        .and_then(|s| s.parse::<f64>().ok())?;
                    let name    = t["details"]["counterparty"]["name"].as_str()
                        .or_else(|| t["description"].as_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    let pending = t["status"].as_str() == Some("pending");
                    let raw     = t["date"].as_str().unwrap_or("");
                    let date    = if raw.len() == 10 {
                        format!("{}/{}", &raw[5..7], &raw[8..10])
                    } else {
                        raw.to_string()
                    };
                    Some(Transaction { amount, name, date, pending })
                }).collect()
            })
            .unwrap_or_default();

        Ok(BankData { balance, transactions })
    }
}

impl Module for BankModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let data  = self.data.lock().unwrap().clone();
        let stale = self.last_ok.lock().unwrap()
            .map(|t| t.elapsed() > STALE_AFTER)
            .unwrap_or(true);

        let bal_h  = measure_text("A", SIZE_PX, false).1 + LINE_GAP;
        let txn_h  = measure_text("A", TXN_SIZE_PX, false).1 + TXN_GAP;
        // Balance line sits one bal_h above the calendar boundary, overlapping the rain area.
        let mut y  = region.y + Y_START - bal_h;

        if stale {
            canvas.fill_rect(region.x, y, region.width * 2 / 5, bal_h, E6Color::Red);
            draw_text(canvas, region.x + MARGIN, y, "(bank offline)", SIZE_PX, E6Color::White, false);
            y += bal_h;
            if data.is_none() { return; }
        }

        let Some(data) = data else { return };

        // Balance line: black on yellow, left third of screen only
        let bal_text = format!("Balance: {}", fmt_dollars(data.balance));
        canvas.fill_rect(region.x, y, region.width * 2 / 5, bal_h, E6Color::Yellow);
        draw_text(canvas, region.x + MARGIN, y, &bal_text, SIZE_PX, E6Color::Black, false);
        y += bal_h;

        // Transaction lines: white on green, smaller font
        // Teller sign: negative = debit (money out), positive = credit (money in)
        for txn in &data.transactions {
            let sign = if txn.amount < 0.0 { "-" } else { "+" };
            let amt  = fmt_dollars(txn.amount.abs());
            let pend = if txn.pending { " P" } else { "" };
            let text = format!("{}  {}{}{}  {}", txn.date, sign, amt, pend, txn.name);
            canvas.fill_rect(region.x, y, region.width, txn_h, E6Color::Green);
            draw_text(canvas, region.x + MARGIN, y, &text, TXN_SIZE_PX, E6Color::White, false);
            y += txn_h;
        }
    }
}

fn fmt_dollars(amount: f64) -> String {
    let total_cents = (amount.abs() * 100.0).round() as u64;
    let dollars     = total_cents / 100;
    let cents       = total_cents % 100;
    let s           = dollars.to_string();
    let len         = s.len();
    let mut out     = String::with_capacity(len + len / 3 + 1);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 { out.push(','); }
        out.push(ch);
    }
    format!("${}.{:02}", out, cents)
}
