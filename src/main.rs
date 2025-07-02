use rand::Rng;
use std::io::{self, Write};
use reqwest::Client;
use scraper::{Html, Selector};
use std::sync::OnceLock;
// use tokio::sync::Semaphore;
use std::sync::Arc;
use tokio::sync::Mutex;
use futures::future::join_all;

// Pre-compile selectors at startup
static VIDEO_SELECTOR: OnceLock<Selector> = OnceLock::new();
static UNAVAILABLE_SELECTOR: OnceLock<Selector> = OnceLock::new();
static TITLE_SELECTOR: OnceLock<Selector> = OnceLock::new();
fn init_selectors() {
    VIDEO_SELECTOR.set(Selector::parse("#movie_player, .html5-video-player").unwrap()).unwrap();
    UNAVAILABLE_SELECTOR.set(Selector::parse("#player-unavailable, .player-unavailable").unwrap()).unwrap();
    TITLE_SELECTOR.set(Selector::parse(r#"meta[property="og:title"]"#).unwrap()).unwrap();
}

struct URLCounter {
    count: usize,
    start_time: std::time::Instant,
    last_printed: std::time::Instant,
} impl URLCounter {
    fn increment(&mut self) {
        self.count += 1;
        let now = std::time::Instant::now();
        
        // Cache the duration check to avoid repeated calculations
        if now.duration_since(self.last_printed).as_secs() >= 1 {
            let elapsed_secs = self.start_time.elapsed().as_secs();
            let urls_per_second = if elapsed_secs > 0 { 
                self.count / elapsed_secs as usize 
            } else { 
                0 
            };
            
            print!("\rURLs: {} | Speed: {} URLs/s | Time: {}s", 
                   self.count, urls_per_second, elapsed_secs);
            io::stdout().flush().unwrap();
            self.last_printed = now;
        }
    }
}

fn generate_url() -> String {
    // YouTube IDs are base64-like but use - and _ instead of + and /
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let random_hash: String = (0..11)
        .map(|_| {
            let idx = rand::rng().random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect();
    format!("https://www.youtube.com/watch?v={}", random_hash)
}

// Lightweight function to check if a YouTube URL has an actual video
async fn check_youtube_video(client: &Client, url: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Accept-Encoding", "gzip, deflate")
        .send()
        .await?;
    
    // Check status code first (faster than parsing HTML)
    match response.status().as_u16() {
        200 => {
            let html = response.text().await?;
            // Only parse if response looks like it contains video data
            if html.contains("ytInitialData") || html.contains("movie_player") {
                let document = Html::parse_document(&html);
                return Ok(document.select(VIDEO_SELECTOR.get().unwrap()).next().is_some());
            }
            Ok(false)
        },
        404 | 410 => Ok(false), // Definitely no video
        _ => Ok(false)
    }
}

#[tokio::main]
async fn main() {
    // Initialize selectors once at startup
    init_selectors();

    // Initialize URL counter
    let counter = Arc::new(Mutex::new(URLCounter {
        count: 0,
        start_time: std::time::Instant::now(),
        last_printed: std::time::Instant::now(),
    }));

    // Create a reqwest client
    let client = Arc::new(Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .pool_max_idle_per_host(50)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()
        .unwrap());
    
    // Limit concurrent requests to avoid overwhelming YouTube if needed
    // let semaphore = Arc::new(Semaphore::new(20));
    
    loop {
        let mut tasks = Vec::new();
        
        // Create batch of concurrent requests
        for _ in 0..50 {
            let client = Arc::clone(&client);
            let counter = Arc::clone(&counter);
            // let semaphore = Arc::clone(&semaphore);
            
            // Create the future directly instead of spawning
            let future = async move {
                // let _permit = semaphore.acquire().await.unwrap();
                let url = generate_url();
                
                match check_youtube_video(&client, &url).await {
                    Ok(true) => Some(url),
                    _ => {
                        counter.lock().await.increment();
                        None
                    }
                }
            };
            tasks.push(future);
        }
        
        // Wait for batch completion
        let results = join_all(tasks).await;
        
        // Check if any valid URL was found
        for result in results {
            if let Some(url) = result {
                println!("\nFound valid YouTube video: {}", url);
                return;
            }
        }
    }
}