use rand::distr::Alphanumeric;
use rand::Rng;
use std::io::{self, Write};
use reqwest::Client;
use scraper::{Html, Selector};
use std::sync::OnceLock;

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
    let random_hash: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(11)
        .map(char::from)
        .collect();
    format!("https://www.youtube.com/watch?v={}", random_hash)
}

// Lightweight function to check if a YouTube URL has an actual video
async fn check_youtube_video(client: &Client, url: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let response = client.get(url).send().await?;
    
    // Check if the response is successful
    if !response.status().is_success() {
        return Ok(false);
    }
    
    // Parse the HTML response
    let html = response.text().await?;
    let document = Html::parse_document(&html);
    
    // Use pre-compiled selectors
    if document.select(VIDEO_SELECTOR.get().unwrap()).next().is_some() {
        return Ok(true);
    }
    
    // Check for "unavailable" elements
    if document.select(UNAVAILABLE_SELECTOR.get().unwrap()).next().is_some() {
        return Ok(false);
    }
    
    // Check for the title meta tag
    if let Some(title_element) = document.select(TITLE_SELECTOR.get().unwrap()).next() {
        if let Some(title) = title_element.value().attr("content") {
            return Ok(!title.contains("not available") && !title.contains("Not Available"));
        }
    }
    
    Ok(false)
}

#[tokio::main]
async fn main() {
    // Initialize selectors once at startup
    init_selectors();

    // Initialize URL counter
    let mut counter = URLCounter {
        count: 0,
        start_time: std::time::Instant::now(),
        last_printed: std::time::Instant::now(),
    };

    // Create a reqwest client
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5)) // Set a reasonable timeout
        .build()
        .unwrap();

    // Starting the YouTube URL search
    loop {
        let url = generate_url();
        
        match check_youtube_video(&client, &url).await {
            Ok(true) => {
                println!("\nFound valid YouTube video: {}", url);
                // Write to file or break if you want to stop at first find
                break;
            }
            Ok(false) => {
                // Video doesn't exist, continue searching
                // DEBUG: println!("\nNo video found at URL: {}", url);
                counter.increment();
            }
            Err(_) => {
                // Network error or parsing error, continue
                println!("\nError checking URL: {}", url);
                counter.increment();
            }
        }
    }
}