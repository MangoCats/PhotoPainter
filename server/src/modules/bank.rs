use std::sync::Mutex;
use std::time::{Duration, Instant};
use chrono::Local;
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use crate::plaid_creds::{CLIENT_ID, SECRET, ACCESS_TOKEN, ACCOUNT_ID};
use super::{Module, Rect};
use super::rain;

const SIZE_PX:     f32 = 28.0;
const MARGIN:      i32 = 8;
const LINE_GAP:    i32 = 4;
const Y_START:     i32 = rain::GCAL_Y_START;
const MAX_TXN:     usize = 5;
const STALE_AFTER: Duration = Duration::from_secs(3600);

// Total lines rendered: 1 balance + up to MAX_TXN transactions.
// Called by renderer to compute where gcal sits below the bank block.
pub fn display_height() -> i32 {
    let ascent = measure_text("A", SIZE_PX, false).1;
    let line_h = ascent + LINE_GAP;
    line_h * (1 + MAX_TXN as i32)
}

#[derive(Clone)]
struct Transaction {
    amount:  f64,    // Plaid convention: positive = debit (out), negative = credit (in)
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
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            data:    Mutex::new(None),
            client,
            last_ok: Mutex::new(None),
        }
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
        let bal_resp: serde_json::Value = self.client
            .post("https://production.plaid.com/accounts/balance/get")
            .json(&serde_json::json!({
                "client_id":    CLIENT_ID,
                "secret":       SECRET,
                "access_token": ACCESS_TOKEN,
                "options": { "account_ids": [ACCOUNT_ID] }
            }))
            .send().await?.json().await?;

        let account = bal_resp["accounts"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or("no accounts in balance response")?;

        // Prefer available balance for checking; fall back to current
        let balance = account["balances"]["available"].as_f64()
            .or_else(|| account["balances"]["current"].as_f64())
            .ok_or("missing balance")?;

        let now        = Local::now();
        let end_date   = now.format("%Y-%m-%d").to_string();
        let start_date = (now - chrono::Duration::days(30)).format("%Y-%m-%d").to_string();

        let txn_resp: serde_json::Value = self.client
            .post("https://production.plaid.com/transactions/get")
            .json(&serde_json::json!({
                "client_id":    CLIENT_ID,
                "secret":       SECRET,
                "access_token": ACCESS_TOKEN,
                "start_date":   start_date,
                "end_date":     end_date,
                "options": {
                    "account_ids": [ACCOUNT_ID],
                    "count":  MAX_TXN,
                    "offset": 0
                }
            }))
            .send().await?.json().await?;

        let transactions = txn_resp["transactions"]
            .as_array()
            .map(|arr| {
                arr.iter().take(MAX_TXN).filter_map(|t| {
                    let amount  = t["amount"].as_f64()?;
                    let name    = t["merchant_name"].as_str()
                        .or_else(|| t["name"].as_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    let pending = t["pending"].as_bool().unwrap_or(false);
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

        let ascent = measure_text("A", SIZE_PX, false).1;
        let line_h = ascent + LINE_GAP;
        let mut y  = region.y + Y_START;

        if stale {
            canvas.fill_rect(region.x, y, region.width, line_h, E6Color::Red);
            draw_text(canvas, region.x + MARGIN, y, "(bank offline)", SIZE_PX, E6Color::White, false);
            y += line_h;
            if data.is_none() { return; }
        }

        let Some(data) = data else { return };

        // Balance line: black on yellow
        let bal_text = format!("Balance: {}", fmt_dollars(data.balance));
        canvas.fill_rect(region.x, y, region.width, line_h, E6Color::Yellow);
        draw_text(canvas, region.x + MARGIN, y, &bal_text, SIZE_PX, E6Color::Black, false);
        y += line_h;

        // Transaction lines: white on green
        for txn in &data.transactions {
            let sign = if txn.amount > 0.0 { "-" } else { "+" };
            let amt  = fmt_dollars(txn.amount.abs());
            let pend = if txn.pending { " P" } else { "" };
            let text = format!("{}  {}{}{}  {}", txn.date, sign, amt, pend, txn.name);
            canvas.fill_rect(region.x, y, region.width, line_h, E6Color::Green);
            draw_text(canvas, region.x + MARGIN, y, &text, SIZE_PX, E6Color::White, false);
            y += line_h;
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
