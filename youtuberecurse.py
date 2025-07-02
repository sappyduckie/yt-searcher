import os
import asyncio
import aiohttp
import random
import string
import time
import multiprocessing
import psutil
import signal
import logging
from concurrent.futures import ThreadPoolExecutor, ProcessPoolExecutor
from collections import deque
import numpy as np

# Configure logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger('youtube_searcher')

# Shared flag for coordination between processes
class SharedFlag:
    def __init__(self):
        self.found = multiprocessing.Value('b', False)
    
    def set(self):
        self.found.value = True
    
    def is_set(self):
        return self.found.value

class AdaptiveBatcher:
    # Dynamically adjusts batch sizes based on performance metrics
    def __init__(self, initial_size=20, min_size=5, max_size=500,
                 increase_factor=1.1, decrease_factor=0.8,
                 measurement_window=5):
        self.batch_size = initial_size
        self.min_size = min_size
        self.max_size = max_size
        self.increase_factor = increase_factor
        self.decrease_factor = decrease_factor
        self.measurement_window = measurement_window
        
        # Performance history
        self.performance_history = deque(maxlen=measurement_window)
        self.last_adjustment_time = time.time()
        self.adjustment_interval = 2.0  # seconds between adjustments
    
    def record_performance(self, batch_size, processing_time, success_rate):
        # Record the performance of a batch processing operation
        if processing_time > 0:
            throughput = batch_size / processing_time
            self.performance_history.append((throughput, success_rate))
            
            # Adjust batch size periodically
            current_time = time.time()
            if current_time - self.last_adjustment_time >= self.adjustment_interval:
                self.adjust_batch_size()
                self.last_adjustment_time = current_time
    
    def adjust_batch_size(self):
        # Adjust batch size based on recent performance metrics
        if not self.performance_history:
            return
        
        # Calculate average throughput and success rate
        avg_throughput = np.mean([p[0] for p in self.performance_history])
        avg_success_rate = np.mean([p[1] for p in self.performance_history])
        
        # Logic to adjust batch size
        if avg_success_rate > 0.98:  # Almost all requests successful
            # Increase batch size to process more at once
            new_size = min(int(self.batch_size * self.increase_factor), self.max_size)
        elif avg_success_rate < 0.8:  # Too many failures
            # Decrease batch size to improve reliability
            new_size = max(int(self.batch_size * self.decrease_factor), self.min_size)
        else:
            # Maintain current size
            new_size = self.batch_size
        
        if new_size != self.batch_size:
            logger.debug(f"Adjusting batch size from {self.batch_size} to {new_size}")
            self.batch_size = new_size
    
    def get_batch_size(self):
        #Get the current recommended batch size
        return self.batch_size

class URLCounter:
    # Thread-safe counter with optimized update intervals
    def __init__(self, update_interval=0.5, batch_size=5000):
        self.count = multiprocessing.Value('i', 0)
        self.lock = multiprocessing.Lock()
        self.start_time = time.time()
        self.last_update = multiprocessing.Value('d', time.time())
        self.last_count = multiprocessing.Value('i', 0)
        self.update_interval = update_interval
        self.pending_increments = multiprocessing.Value('i', 0)
        self.batch_size = batch_size
        
    def increment(self, amount=1):
        # Use atomic operations when possible to avoid locks
        with self.pending_increments.get_lock():
            self.pending_increments.value += amount
        
        # Only update the display when we've reached batch size or interval has passed
        current_time = time.time()
        with self.last_update.get_lock():
            elapsed = current_time - self.last_update.value
            if (self.pending_increments.value >= self.batch_size or elapsed >= self.update_interval):
                # Update the total count
                with self.lock:
                    self.count.value += self.pending_increments.value
                    self.pending_increments.value = 0
                
                if elapsed >= self.update_interval:
                    with self.last_count.get_lock():
                        urls_per_second = (self.count.value - self.last_count.value) / elapsed
                        total_urls = self.count.value
                        print(f"\rChecked: {total_urls:,} | Speed: {urls_per_second:.0f} URLs/s", end='')
                        self.last_count.value = self.count.value
                        self.last_update.value = current_time

# Optimized hash generation using NumPy for better performance
def generate_hash_batch(batch_size=10000, length=11):
    # Generate YouTube-compatible hashes using NumPy for efficiency
    chars = np.array(list(string.ascii_letters + string.digits))
    # Generate random indices into the chars array
    indices = np.random.randint(0, len(chars), size=(batch_size, length))
    # Use vectorized operations for better performance
    result = [''.join(chars[idx]) for idx in indices]
    return result

class LockFreeQueue:
    # A mostly lock-free queue implementation using a ring buffer
    def __init__(self, maxsize=50000):
        self.buffer = multiprocessing.Array('i', maxsize)
        self.maxsize = maxsize
        self.head = multiprocessing.Value('i', 0)
        self.tail = multiprocessing.Value('i', 0)
        
    def put_nowait(self, item):
        # Put an item in the queue without blocking
        with self.head.get_lock():
            next_head = (self.head.value + 1) % self.maxsize
            if next_head == self.tail.value:
                return False  # Queue is full
            self.buffer[self.head.value] = item
            self.head.value = next_head
            return True
            
    def get_nowait(self):
        # Get an item from the queue without blocking
        with self.tail.get_lock():
            if self.tail.value == self.head.value:
                return None  # Queue is empty
            item = self.buffer[self.tail.value]
            self.tail.value = (self.tail.value + 1) % self.maxsize
            return item
    
    def qsize(self):
        # Approximate size of the queue
        head = self.head.value
        tail = self.tail.value
        if head >= tail:
            return head - tail
        return self.maxsize - (tail - head)

class URLFileWriter:
    # Efficient file writer with batched operations
    def __init__(self, filename, buffer_size=100):
        self.filename = filename
        self.buffer_size = buffer_size
        self.buffer = []
        self.lock = multiprocessing.Lock()
        
        # Create the file if it doesn't exist
        if not os.path.exists(filename):
            with open(filename, "w") as f:
                pass
    
    def add_url(self, url):
        with self.lock:
            self.buffer.append(f"found: {url}\n")
            if len(self.buffer) >= self.buffer_size:
                self.flush()
    
    def flush(self):
        with self.lock:
            if self.buffer:
                with open(self.filename, "a") as f:
                    f.writelines(self.buffer)
                self.buffer.clear()

def process_worker(process_id, shared_flag, url_counter, url_file_writer):
    # Worker function that runs in a separate process
    # Configure process-specific settings
    asyncio.set_event_loop(asyncio.new_event_loop())
    
    # Set process name for easier monitoring
    try:
        process = psutil.Process()
        process.name(f"youtube_worker_{process_id}")
    except Exception:
        pass
    
    # Handle signals gracefully
    def handle_signal(sig, frame):
        logger.info(f"Process {process_id} received signal {sig}, shutting down...")
        loop = asyncio.get_event_loop()
        if loop.is_running():
            loop.stop()
    
    signal.signal(signal.SIGINT, handle_signal)
    signal.signal(signal.SIGTERM, handle_signal)
    
    # Start the async event loop
    loop = asyncio.get_event_loop()
    try:
        loop.run_until_complete(async_worker_main(process_id, shared_flag, url_counter, url_file_writer))
    finally:
        loop.close()

async def async_worker_main(process_id, shared_flag, url_counter, url_file_writer):
    # Main async function for each worker process
    # Create an adaptive batcher for this process
    batcher = AdaptiveBatcher(
        initial_size=20,
        min_size=5,
        max_size=200,
        increase_factor=1.2,
        decrease_factor=0.8
    )
    
    # Use process-specific connection pooling
    connector = aiohttp.TCPConnector(
        limit=0,                 # No connection limit
        ttl_dns_cache=600,       # Cache DNS for 10 minutes
        force_close=False,       # Keep connections open
        enable_cleanup_closed=True,
        ssl=False                # YouTube doesn't require SSL validation for this purpose
    )
    
    timeout = aiohttp.ClientTimeout(
        total=5,                 # Total timeout
        connect=1,               # Connection timeout
        sock_connect=1,          # Socket connect timeout
        sock_read=3              # Socket read timeout
    )
    
    # Calculate optimal number of worker tasks based on CPU cores
    cpu_count = os.cpu_count() or 4
    worker_tasks = max(5, cpu_count * 2)  # 2 tasks per CPU core
    
    # Pre-generate some URLs to start with
    url_queue = deque()
    executor = ThreadPoolExecutor(max_workers=1)
    
    # Initial URL batch
    urls = await asyncio.get_event_loop().run_in_executor(
        executor, generate_hash_batch, 5000, 11
    )
    for hash_val in urls:
        url_queue.append(f"https://youtu.be/{hash_val}")
    
    logger.info(f"Process {process_id} initialized with {worker_tasks} worker tasks")
    
    async with aiohttp.ClientSession(
        connector=connector, 
        timeout=timeout,
        headers={
            'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
            'Accept': '*/*',
            'Connection': 'keep-alive'
        }
    ) as session:
        tasks = []
        
        # Background task to keep the URL queue filled
        async def url_generator():
            while not shared_flag.is_set():
                if len(url_queue) < 1000:
                    urls = await asyncio.get_event_loop().run_in_executor(
                        executor, generate_hash_batch, 5000, 11
                    )
                    for hash_val in urls:
                        url_queue.append(f"https://youtu.be/{hash_val}")
                await asyncio.sleep(0.1)
        
        # Start the URL generator task
        url_gen_task = asyncio.create_task(url_generator())
        
        async def check_url(url):
            try:
                async with session.get(url) as response:
                    if response.status == 200:
                        content_type = response.headers.get('content-type', '')
                        if 'image' in content_type:
                            url_file_writer.add_url(url)
                            logger.info(f"Found valid URL: {url}")
                            shared_flag.set()
                            return True
                    return False
            except (aiohttp.ClientError, asyncio.TimeoutError):
                return False
        
        async def worker_task():
            while not shared_flag.is_set():
                # Get the current recommended batch size
                batch_size = batcher.get_batch_size()
                
                # Get batch of URLs to process
                urls = []
                for _ in range(min(batch_size, len(url_queue))):
                    if url_queue:
                        urls.append(url_queue.popleft())
                    else:
                        break
                
                if not urls:
                    await asyncio.sleep(0.01)
                    continue
                
                # Process the batch
                start_time = time.time()
                try:
                    tasks = [check_url(url) for url in urls]
                    results = await asyncio.gather(*tasks)
                    
                    # Record performance metrics
                    processing_time = time.time() - start_time
                    success_rate = 1.0 - (sum(1 for r in results if not r) / len(results))
                    batcher.record_performance(len(urls), processing_time, success_rate)
                    
                    # Update the counter
                    url_counter.increment(len(urls))
                    
                    # Check if we found a valid URL
                    if any(results):
                        return
                        
                except Exception as e:
                    logger.error(f"Error in worker task: {e}")
        
        # Start worker tasks
        workers = [asyncio.create_task(worker_task()) for _ in range(worker_tasks)]
        
        # Wait for any worker to find a URL or all to finish
        done, pending = await asyncio.wait(
            workers, 
            return_when=asyncio.FIRST_COMPLETED
        )
        
        # Cancel pending tasks
        url_gen_task.cancel()
        for task in pending:
            task.cancel()
        
        # Wait for tasks to be properly cancelled
        await asyncio.gather(*pending, return_exceptions=True)
        
        # Flush any remaining URLs to disk
        url_file_writer.flush()

def main():
    print("\nAdvanced YouTube URL Searcher")
    print("Searching for valid YouTube URLs...")
    print("This may take a while...\n")
    
    # Determine optimal number of processes based on CPU cores
    cpu_count = 24
    num_processes = max(1, cpu_count - 1)  # Leave one core free for system
    
    # Create shared objects
    shared_flag = SharedFlag()
    url_counter = URLCounter(update_interval=0.5, batch_size=5000)
    url_file_writer = URLFileWriter("urls.txt", buffer_size=50)
    
    # Start processes
    processes = []
    for i in range(num_processes):
        p = multiprocessing.Process(
            target=process_worker,
            args=(i, shared_flag, url_counter, url_file_writer)
        )
        p.daemon = True
        processes.append(p)
        p.start()
        # Stagger process starts to avoid overwhelming the system
        time.sleep(0.5)
    
    try:
        # Monitor for completion
        while True:
            if shared_flag.is_set():
                print("\nValid URL found! Shutting down...")
                break
            
            # Check if any process died
            for i, p in enumerate(processes):
                if not p.is_alive():
                    # Restart the process
                    print(f"\nProcess {i} died, restarting...")
                    p = multiprocessing.Process(
                        target=process_worker,
                        args=(i, shared_flag, url_counter, url_file_writer)
                    )
                    p.daemon = True
                    processes[i] = p
                    p.start()
            
            time.sleep(1)
    except KeyboardInterrupt:
        print("\nInterrupted by user. Shutting down...")
    finally:
        # Clean shutdown
        for p in processes:
            p.terminate()
        
        # Final flush of URL buffer
        url_file_writer.flush()
        print("\nSearch complete. Check urls.txt for results.")

if __name__ == "__main__":
    multiprocessing.freeze_support()  # Required for Windows
    main()
