const AudioPlayerManager = require('./audio-player');

class TTSPlayer {
  constructor(options = {}) {
    this.player = new AudioPlayerManager({
      defaultVolume: options.volume || 100,
      maxQueueSize: options.maxQueueSize || 20,
      autoPlay: true,
    });
    this.ttsConfig = {
      endpoint: options.endpoint || process.env.CHAMPR_TTS_ENDPOINT || '',
      voice: options.voice || process.env.CHAMPR_TTS_VOICE || 'zh-CN-XiaoxiaoNeural',
      format: options.format || 'audio-24khz-48kbitrate-mono-mp3',
      ...options.ttsConfig,
    };
  }

  async speak(text, options = {}) {
    const ttsOptions = { ...this.ttsConfig, ...options };
    if (!ttsOptions.endpoint) {
      throw new Error('TTS endpoint is not configured');
    }

    const response = await fetch(ttsOptions.endpoint, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        text,
        voice: ttsOptions.voice,
        format: ttsOptions.format,
      }),
    });

    if (!response.ok) {
      throw new Error(`TTS request failed with status ${response.status}`);
    }

    const audioBuffer = Buffer.from(await response.arrayBuffer());
    await this.player.playBuffer(audioBuffer, {
      volume: ttsOptions.volume,
      loop: ttsOptions.loop,
      enqueue: ttsOptions.enqueue,
    });

    return { success: true, text, audioSize: audioBuffer.length };
  }

  async stop() {
    await this.player.stop();
  }

  clearQueue() {
    this.player.clearQueue();
  }
}

module.exports = TTSPlayer;
