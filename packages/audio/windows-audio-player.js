const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

class WindowsAudioPlayer {
  constructor() {
    this.isWindows = process.platform === 'win32';
    this.currentProcess = null;
    this.tempFiles = [];
  }

  async playWithMCI(audioBuffer) {
    const tempFile = await this.createTempFile(audioBuffer);
    const durationMs = Math.max(1000, Math.ceil((audioBuffer.length * 8 * 1000) / 48000) + 500);
    const psScript = `
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public class MCI
{
    [DllImport("winmm.dll")]
    private static extern long mciSendString(string command, StringBuilder returnValue, int returnLength, IntPtr hwndCallback);

    public static void SendString(string command)
    {
        mciSendString(command, null, 0, IntPtr.Zero);
    }

    public static string GetString(string command)
    {
        var builder = new StringBuilder(256);
        mciSendString(command, builder, builder.Capacity, IntPtr.Zero);
        return builder.ToString();
    }
}
"@
[MCI]::SendString('open "${tempFile}" type mpegvideo alias champraudio')
[MCI]::SendString('play champraudio')
Start-Sleep -Milliseconds ${durationMs}
[MCI]::SendString('close champraudio')
exit 0
`;

    return this.executePowerShell(psScript, tempFile);
  }

  async playWithWpf(audioBuffer) {
    const tempFile = await this.createTempFile(audioBuffer);
    const durationMs = Math.max(1000, Math.ceil((audioBuffer.length * 8 * 1000) / 48000) + 500);
    const fileUri = `file:///${tempFile.replace(/\\/g, '/')}`;
    const psScript = `
Add-Type -AssemblyName PresentationCore
$player = New-Object System.Windows.Media.MediaPlayer
$player.Open([System.Uri]'${fileUri}')
$player.Play()
Start-Sleep -Milliseconds ${durationMs}
$player.Close()
exit 0
`;
    return this.executePowerShell(psScript, tempFile);
  }

  async executePowerShell(script, tempFile) {
    return new Promise((resolve, reject) => {
      const ps = spawn(
        'powershell',
        ['-NoProfile', '-WindowStyle', 'Hidden', '-ExecutionPolicy', 'Bypass', '-Command', script],
        { windowsHide: true, stdio: 'ignore' },
      );

      this.currentProcess = ps;
      let timeoutHandle = null;

      ps.on('close', (code) => {
        if (timeoutHandle) clearTimeout(timeoutHandle);
        this.cleanupTempFile(tempFile);
        if (code === 0) resolve();
        else reject(new Error(`MCI playback failed with code ${code}`));
      });
      ps.on('error', (error) => {
        if (timeoutHandle) clearTimeout(timeoutHandle);
        this.cleanupTempFile(tempFile);
        reject(error);
      });

      timeoutHandle = setTimeout(() => {
        if (!ps.killed) {
          ps.kill();
          this.cleanupTempFile(tempFile);
          reject(new Error('MCI playback timeout'));
        }
      }, 30000);
    });
  }

  async createTempFile(audioBuffer) {
    const tempDir = path.join(os.tmpdir(), 'champr-audio');
    if (!fs.existsSync(tempDir)) fs.mkdirSync(tempDir, { recursive: true });
    const tempFile = path.join(tempDir, `audio-${Date.now()}.mp3`);
    fs.writeFileSync(tempFile, audioBuffer);
    this.tempFiles.push(tempFile);
    return tempFile;
  }

  cleanupTempFile(filePath) {
    try {
      if (fs.existsSync(filePath)) fs.unlinkSync(filePath);
      this.tempFiles = this.tempFiles.filter((file) => file !== filePath);
    } catch {
      // ignore cleanup errors
    }
  }

  async stop() {
    if (this.currentProcess && !this.currentProcess.killed) this.currentProcess.kill();
    this.tempFiles.forEach((file) => this.cleanupTempFile(file));
    this.tempFiles = [];
  }

  async play(audioBuffer, options = {}) {
    if (!this.isWindows) throw new Error('This player is Windows-only');
    try {
      return await this.playWithWpf(audioBuffer);
    } catch {
      return this.playWithMCI(audioBuffer);
    }
  }
}

module.exports = WindowsAudioPlayer;
