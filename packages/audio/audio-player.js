const { spawn, execSync } = require('child_process');
const { EventEmitter } = require('events');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { Readable } = require('stream');

class AudioPlayerManager extends EventEmitter {
  constructor(options = {}) {
    super();
    this.options = {
      defaultVolume: 100,
      maxQueueSize: 10,
      autoPlay: true,
      ...options,
    };
    this.queue = [];
    this.currentPlayer = null;
    this.isPlaying = false;
    this.platform = process.platform;
    this.availablePlayers = this.detectAvailablePlayers();
  }

  detectAvailablePlayers() {
    const players = [];
    const candidates = [
      { name: 'ffplay', args: ['-i', 'pipe:0', '-nodisp', '-autoexit', '-loglevel', 'quiet'] },
      { name: 'mpv', args: ['--no-video', '--really-quiet', '-'] },
      { name: 'mplayer', args: ['-really-quiet', '-'] },
      { name: 'aplay', args: ['-q'] },
      { name: 'afplay', args: [] },
      { name: 'powershell', args: [] },
    ];

    for (const candidate of candidates) {
      if (this.commandExists(candidate.name)) {
        players.push(candidate);
      }
    }

    return players;
  }

  commandExists(cmd) {
    try {
      const checkCmd = process.platform === 'win32' ? `where ${cmd}` : `which ${cmd}`;
      execSync(checkCmd, { stdio: 'ignore' });
      return true;
    } catch {
      return false;
    }
  }

  async playFile(filePath, options = {}) {
    if (!fs.existsSync(filePath)) {
      throw new Error(`Audio file not found: ${filePath}`);
    }
    const buffer = fs.readFileSync(filePath);
    return this.playBuffer(buffer, { ...options, sourceType: 'file', sourcePath: filePath });
  }

  async playBuffer(audioBuffer, options = {}) {
    const playOptions = { volume: this.options.defaultVolume, autoPlay: this.options.autoPlay, ...options };
    if (playOptions.enqueue) {
      return this.enqueue(audioBuffer, playOptions);
    }
    if (this.isPlaying) {
      await this.stop();
    }
    return this.play(audioBuffer, playOptions);
  }

  enqueue(audioBuffer, options) {
    return new Promise((resolve, reject) => {
      if (this.queue.length >= this.options.maxQueueSize) {
        reject(new Error('Queue is full'));
        return;
      }
      this.queue.push({ buffer: audioBuffer, options, resolve, reject });
      this.emit('queued', { queueLength: this.queue.length });
      if (!this.isPlaying) {
        this.processQueue();
      }
    });
  }

  async processQueue() {
    if (this.queue.length === 0) {
      this.isPlaying = false;
      this.emit('queue-empty');
      return;
    }

    this.isPlaying = true;
    const item = this.queue.shift();
    this.emit('queue-start', { queueLength: this.queue.length, options: item.options });
    try {
      await this.play(item.buffer, item.options);
      item.resolve();
    } catch (error) {
      item.reject(error);
    } finally {
      this.processQueue();
    }
  }

  async play(audioBuffer, options) {
    if (this.availablePlayers.length === 0) {
      throw new Error('No audio player available. Install ffplay, mpv, mplayer, or use Windows PowerShell.');
    }
    const player = this.selectBestPlayer(options);
    return new Promise((resolve, reject) => {
      try {
        if (player.name === 'powershell') {
          this.playWithPowerShell(audioBuffer, options, resolve, reject);
        } else {
          this.playWithPipe(player, audioBuffer, options, resolve, reject);
        }
      } catch (error) {
        reject(error);
      }
    });
  }

  selectBestPlayer() {
    const priority = ['ffplay', 'mpv', 'mplayer', 'aplay', 'afplay', 'powershell'];
    for (const name of priority) {
      const player = this.availablePlayers.find((p) => p.name === name);
      if (player) return player;
    }
    return this.availablePlayers[0];
  }

  playWithPipe(player, audioBuffer, options, resolve, reject) {
    const args = this.buildPlayerArgs(player, options);
    const childProcess = spawn(player.name, args, { stdio: ['pipe', 'ignore', 'pipe'] });
    this.currentPlayer = childProcess;

    const stream = Readable.from(audioBuffer);
    stream.pipe(childProcess.stdin);
    childProcess.stderr.on('data', (data) => {
      const message = data.toString();
      if (!this.isQuietError(message)) {
        this.emit('player-error', { message });
      }
    });
    childProcess.on('error', (error) => {
      this.cleanup();
      reject(error);
    });
    childProcess.on('close', (code) => {
      this.cleanup();
      if (code === 0 || code === null) {
        this.emit('playback-complete', { options });
        resolve();
      } else {
        reject(new Error(`Playback failed with exit code ${code}`));
      }
    });
    stream.on('end', () => childProcess.stdin.end());
  }

  async playWithPowerShell(audioBuffer, options, resolve, reject) {
    const tempFile = path.join(os.tmpdir(), `champr-audio-${Date.now()}-${Math.random().toString(36).slice(2)}.wav`);
    try {
      fs.writeFileSync(tempFile, audioBuffer);
      const psScript = `
$player = New-Object System.Media.SoundPlayer
$player.SoundLocation = '${tempFile.replace(/'/g, "''")}'
$player.Load()
$player.PlaySync()
`;
      const ps = spawn('powershell', ['-NoProfile', '-Command', psScript]);
      this.currentPlayer = ps;
      ps.on('close', (code) => {
        fs.rmSync(tempFile, { force: true });
        this.cleanup();
        if (code === 0) {
          this.emit('playback-complete', { options });
          resolve();
        } else {
          reject(new Error(`PowerShell playback failed with exit code ${code}`));
        }
      });
      ps.on('error', reject);
    } catch (error) {
      fs.rmSync(tempFile, { force: true });
      reject(error);
    }
  }

  buildPlayerArgs(player, options) {
    const args = [...player.args];
    if (options.volume !== undefined && options.volume !== 100) {
      if (player.name === 'ffplay') args.push('-volume', String(Math.max(0, Math.min(100, options.volume))));
      if (player.name === 'mpv') args.push(`--volume=${Math.max(0, Math.min(100, options.volume))}`);
    }
    if (options.loop) {
      if (player.name === 'ffplay') args.push('-loop', '0');
      if (player.name === 'mpv') args.push('--loop=inf');
    }
    if (options.startTime !== undefined) {
      if (player.name === 'ffplay') args.push('-ss', String(options.startTime));
      if (player.name === 'mpv') args.push(`--start=${options.startTime}`);
    }
    return args;
  }

  async stop() {
    if (this.currentPlayer) {
      const player = this.currentPlayer;
      this.currentPlayer = null;
      try {
        player.kill('SIGTERM');
        await new Promise((resolve) => setTimeout(resolve, 100));
        if (!player.killed) player.kill('SIGKILL');
      } catch {
        // already exited
      }
      this.isPlaying = false;
      this.emit('stopped');
    }
  }

  clearQueue() {
    const clearedItems = this.queue.splice(0);
    clearedItems.forEach((item) => item.reject(new Error('Queue cleared')));
    this.emit('queue-cleared', { clearedCount: clearedItems.length });
  }

  cleanup() {
    this.currentPlayer = null;
    this.isPlaying = false;
  }

  isQuietError(message) {
    const patterns = ['loglevel', 'ffplay version', 'configuration:', 'libav', 'built with', 'Input #0', 'Duration:', 'Stream #', 'Metadata:', 'title:', 'artist:', 'album:', 'encoder:'];
    return patterns.some((pattern) => message.toLowerCase().includes(pattern.toLowerCase()));
  }
}

module.exports = AudioPlayerManager;
