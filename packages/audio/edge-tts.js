const { MsEdgeTTS, OUTPUT_FORMAT } = require('msedge-tts');

async function synthesize(text, options = {}) {
  const voice = options.voice || process.env.CHAMPR_TTS_VOICE || 'zh-CN-XiaoxiaoNeural';
  const tts = new MsEdgeTTS({
    voiceName: voice,
    outputFormat: OUTPUT_FORMAT.AUDIO_24KHZ_48KBITRATE_MONO_MP3,
  });
  await tts.setMetadata(voice, OUTPUT_FORMAT.AUDIO_24KHZ_48KBITRATE_MONO_MP3);

  const { audioStream } = await tts.toStream(text);
  const chunks = [];
  for await (const chunk of audioStream) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  return Buffer.concat(chunks);
}

module.exports = { synthesize };
