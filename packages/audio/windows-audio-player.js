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
}
"@
[MCI]::SendString('open "${tempFile}" type mpegvideo alias champraudio')
[MCI]::SendString('play champraudio wait')
[MCI]::SendString('close champraudio')
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

      ps.on('close', (code) => {
        this.cleanupTempFile(tempFile);
        if (code === 0) resolve();
        else reject(new Error(`MCI playback failed with code ${code}`));
      });
      ps.on('error', (error) => {
        this.cleanupTempFile(tempFile);
        reject(error);
      });

      setTimeout(() => {
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
    return this.playWithMCI(audioBuffer);
  }
}

module.exports = WindowsAudioPlayer;
