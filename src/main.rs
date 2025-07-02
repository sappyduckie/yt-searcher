// Import necessary libraries for our YouTube URL searcher
use rand::Rng;                                    // For generating random numbers/characters
use std::io::{self, Write};                       // For console output and flushing stdout
use reqwest::Client;                              // HTTP client for making web requests
use std::sync::Arc;                               // For sharing data between threads safely
use tokio::sync::mpsc;                            // Multi-producer, single-consumer channel for async communication
use std::sync::atomic::{AtomicUsize, Ordering};   // Atomic counter for thread-safe counting without locks
 
/*
 * Generates a random YouTube URL with a valid video ID format
 * 
 * YouTube video IDs are 11 characters long and use base64-like encoding
 * with characters A-Z, a-z, 0-9, and the special characters - and _
 * 
 * Returns: A string in the format "https://www.youtube.com/watch?v=XXXXXXXXXXX"
 */
fn generate_url() -> String {
    // Define the character set used in YouTube video IDs
    // This matches YouTube's actual character set for video IDs
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    
    // Get a thread-local random number generator for performance
    let mut rng = rand::rng();
    
    // Generate an 11-character random string by:
    // 1. Creating a range from 0 to 11 (YouTube video IDs are exactly 11 chars)
    // 2. For each position, pick a random character from our charset
    // 3. Convert the byte to a char and collect into a String
    let hash: String = (0..11)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect();
    
    // Format the random hash into a proper YouTube watch URL
    format!("https://www.youtube.com/watch?v={}", hash)
}

/*
 * Checks if a YouTube URL actually contains a valid, accessible video
 * 
 * This function performs a lightweight check by:
 * 1. Making an HTTP GET request to the URL
 * 2. Checking the response status code
 * 3. Analyzing the HTML content for video indicators
 * 4. Avoiding expensive HTML parsing by using simple string searches
 * 
 * Args:
 *   client: Shared HTTP client for making requests
 *   url: The YouTube URL to check
 * 
 * Returns: 
 *   Ok(true) if the URL contains a valid video
 *   Ok(false) if no video is found or video is unavailable
 *   Err if there's a network or parsing error
 */
async fn check_youtube_video(client: &Client, url: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    // Make HTTP GET request to the YouTube URL
    // We set a realistic User-Agent to avoid being blocked as a bot
    let response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .send()
        .await?;

    // Quick check: if the response isn't HTTP 200 OK, there's no video
    // This catches deleted videos, private videos, etc. without parsing HTML
    if response.status() != 200 {
        return Ok(false);
    }

    // Get the full HTML response body
    // This is necessary to check for video content indicators
    let html = response.text().await?;
    
    // Check for common "video unavailable" indicators in the HTML
    // These strings appear when videos are deleted, private, or region-blocked
    // This is much faster than parsing the full HTML structure
    if html.contains("Video unavailable") || 
       html.contains("This video is not available") ||
       html.contains("Private video") ||
       html.contains("This video has been removed") {
        return Ok(false);
    }
    
    // Check for positive indicators that a video exists and is playable
    // ytInitialData: JavaScript object containing video metadata
    // videoDetails: Specific section with video information
    // watch?v=: Confirms this is a watch page (not a channel, playlist, etc.)
    Ok(html.contains("ytInitialData") && 
       (html.contains("videoDetails") || html.contains("watch?v=")))
}

/*
 * Main function: Orchestrates the YouTube URL search operation
 * 
 * This program uses a brute-force approach to find valid YouTube videos by:
 * 1. Generating millions of random YouTube URLs
 * 2. Testing each URL concurrently across many worker tasks
 * 3. Stopping as soon as we find the first valid video
 * 
 * The approach is highly parallelized to maximize throughput, using:
 * - Atomic counters for thread-safe counting without locks
 * - Hundreds of concurrent worker tasks
 * - Optimized HTTP client settings
 * - Async/await for non-blocking I/O operations
 */
#[tokio::main(flavor = "multi_thread")]
async fn main() {
    
    // Create an atomic counter to track how many URLs we've checked
    // AtomicUsize allows multiple threads to increment safely without mutex locks
    // This is much faster than using a Mutex<usize> for high-frequency updates
    let counter = Arc::new(AtomicUsize::new(0));

    // Create and configure the HTTP client for optimal performance
    // Arc allows us to share the same client across all worker threads
    let client = Arc::new(Client::builder()
        .timeout(std::time::Duration::from_millis(800))     // Max 800ms per request
        .connect_timeout(std::time::Duration::from_millis(500)) // Max 500ms to establish connection
        .pool_max_idle_per_host(0)                          // Unlimited connection pooling
        .pool_idle_timeout(std::time::Duration::from_secs(20))  // Keep connections alive for 20s
        .tcp_keepalive(std::time::Duration::from_secs(30))      // TCP keepalive every 30s
        .tcp_nodelay(true)                                      // Disable Nagle's algorithm for lower latency
        .http2_prior_knowledge()                                // Use HTTP/2 when possible for better performance
        .build()
        .unwrap());

    // Create a channel for communication between worker threads and main thread
    // Workers will send successful URLs through this channel
    // mpsc = Multi-Producer, Single-Consumer (many workers, one main thread)
    let (tx, mut rx) = mpsc::unbounded_channel();
    
    // Calculate how many worker threads to spawn
    // We use CPU count × 500 because this is I/O bound work (network requests)
    // I/O bound tasks can have many more threads than CPU cores since they spend
    // most time waiting for network responses, not using CPU
    let cpu_count = std::thread::available_parallelism().unwrap().get();
    let num_workers = cpu_count * 1000;
    
    println!("Starting {} workers on {} CPUs", num_workers, cpu_count);
    
    // Spawn a dedicated task for displaying progress statistics
    // This runs independently and updates the console every second
    let counter_clone = Arc::clone(&counter);
    tokio::spawn(async move {
        let mut last_count = 0;                    // URLs checked at last update
        let mut last_time = std::time::Instant::now(); // Time of last update
        
        // Infinite loop to continuously update progress display
        loop {
            // Wait 1 second between updates to avoid spamming the console
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            
            // Get current statistics
            let current_count = counter_clone.load(Ordering::Relaxed);  // Total URLs checked
            let now = std::time::Instant::now();                       // Current time
            let elapsed = now.duration_since(last_time).as_secs_f64(); // Time since last update
            
            // Calculate URLs per second since last update
            let rate = (current_count - last_count) as f64 / elapsed;
            
            // Print progress on same line (\r returns cursor to beginning of line)
            // This creates a live-updating display instead of printing new lines
            print!("\rChecked: {} | Speed: {:.0} URLs/s", current_count, rate);
            io::stdout().flush().unwrap(); // Force immediate output to console
            
            // Update tracking variables for next iteration
            last_count = current_count;
            last_time = now;
        }
    });
    
    // Spawn all the worker tasks that will do the actual URL testing
    // Each worker runs independently and concurrently with others
    for _ in 0..num_workers {
        // Clone the shared resources for this worker
        // Arc::clone creates a new reference to the same data, not a deep copy
        let client = Arc::clone(&client);     // HTTP client for making requests
        let counter = Arc::clone(&counter);   // Shared counter for statistics
        let tx = tx.clone();                  // Channel sender for reporting success
        
        // Spawn an async task that will run on the tokio thread pool
        // Each task is independent and runs the worker loop
        tokio::spawn(async move {
            // Worker main loop: generate and test URLs until we find a valid one
            loop {
                // Generate a random YouTube URL to test
                let url = generate_url();
                
                // Test if this URL contains a valid video
                match check_youtube_video(&client, &url).await {
                    Ok(true) => {
                        // Success! We found a valid video
                        // Send the URL through the channel to the main thread
                        let _ = tx.send(url);
                        // Exit this worker - our job is done
                        return;
                    }
                    _ => {
                        // No video found (or error occurred)
                        // Increment the counter to track our progress
                        // fetch_add atomically adds 1 and returns the previous value
                        // Ordering::Relaxed is fastest - we don't need strict ordering
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // Loop continues to try the next URL
            }
        });
    }
    
    // Close the sending side of the channel
    // This allows the receiving side to know when all workers are done
    // (though in practice, we expect to find a video before that happens)
    drop(tx);
    
    // Wait for the first worker to find a valid YouTube video
    // rx.recv() blocks until a message is received through the channel
    if let Some(url) = rx.recv().await {
        // Print a newline first to avoid overwriting the progress display
        // Then show the successful URL
        println!("\nFound valid YouTube video: {}", url);
    }
}