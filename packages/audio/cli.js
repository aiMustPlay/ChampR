const TTSPlayer = require('./tts-player');
const AudioPlayerManager = require('./audio-player');
const edgeTts = require('./edge-tts');
const WindowsAudioPlayer = require('./windows-audio-player');

function parseArgs(argv) {
  const args = argv.slice(2);
  const options = {};
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    const next = args[i + 1];
    if (arg === '--text') options.text = next;
    if (arg.startsWith('--text=')) options.text = arg.split('=')[1];
    if (arg === '--voice') options.voice = next;
    if (arg.startsWith('--voice=')) options.voice = arg.split('=')[1];
    if (arg === '--endpoint') options.endpoint = next;
    if (arg.startsWith('--endpoint=')) options.endpoint = arg.split('=')[1];
    if (arg === '--volume') options.volume = Number(next);
    if (arg.startsWith('--volume=')) options.volume = Number(arg.split('=')[1]);
  }
  return options;
}

async function main() {
  const options = parseArgs(process.argv);
  if (!options.text) {
    console.error('Usage: node cli.js --text="..." [--voice=zh-CN-XiaoxiaoNeural] [--endpoint=...]');
    process.exit(1);
  }

  let result;
  if (options.endpoint) {
    const player = new TTSPlayer(options);
    result = await player.speak(options.text);
  } else {
    const audio = await edgeTts.synthesize(options.text, options);
    const player = new WindowsAudioPlayer();
    await player.play(audio, { method: 'mci' });
    result = { success: true, text: options.text, audioSize: audio.length, engine: 'edge-neural' };
  }

  console.log(JSON.stringify(result));
  process.exit(0);
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
