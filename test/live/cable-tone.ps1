param([double]$Seconds = 5, [double]$Freq = 880, [double]$AmplitudeDb = -20)
# Plays a tone into the VB-Audio virtual cable: it reaches the "CABLE Output" recording device and no speaker.
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices; using System.Threading;
public static class CableTone {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Auto)]
  struct WAVEOUTCAPS { public ushort wMid, wPid; public uint vDriverVersion; [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string szPname; public uint dwFormats; public ushort wChannels, wReserved1; public uint dwSupport; }
  [StructLayout(LayoutKind.Sequential)]
  struct WAVEFORMATEX { public ushort wFormatTag, nChannels; public uint nSamplesPerSec, nAvgBytesPerSec; public ushort nBlockAlign, wBitsPerSample, cbSize; }
  [StructLayout(LayoutKind.Sequential)]
  struct WAVEHDR { public IntPtr lpData; public uint dwBufferLength, dwBytesRecorded; public IntPtr dwUser; public uint dwFlags, dwLoops; public IntPtr lpNext, reserved; }
  [DllImport("winmm.dll", CharSet = CharSet.Auto)] static extern uint waveOutGetDevCaps(IntPtr id, ref WAVEOUTCAPS caps, uint size);
  [DllImport("winmm.dll")] static extern uint waveOutGetNumDevs();
  [DllImport("winmm.dll")] static extern uint waveOutOpen(out IntPtr h, IntPtr id, ref WAVEFORMATEX f, IntPtr cb, IntPtr inst, uint flags);
  [DllImport("winmm.dll")] static extern uint waveOutPrepareHeader(IntPtr h, IntPtr hdr, uint size);
  [DllImport("winmm.dll")] static extern uint waveOutWrite(IntPtr h, IntPtr hdr, uint size);
  [DllImport("winmm.dll")] static extern uint waveOutUnprepareHeader(IntPtr h, IntPtr hdr, uint size);
  [DllImport("winmm.dll")] static extern uint waveOutClose(IntPtr h);
  [DllImport("winmm.dll")] static extern uint waveOutReset(IntPtr h);
  public static string Play(double seconds, double freq, double db) {
    int device = -1; uint count = waveOutGetNumDevs();
    for (uint i = 0; i < count; i++) {
      var caps = new WAVEOUTCAPS();
      if (waveOutGetDevCaps(new IntPtr(i), ref caps, (uint)Marshal.SizeOf(caps)) == 0 && caps.szPname.IndexOf("CABLE Input", StringComparison.OrdinalIgnoreCase) >= 0) { device = (int)i; break; }
    }
    if (device < 0) return "no CABLE Input among " + count + " devices";
    int rate = 48000, n = (int)(rate * seconds);
    var pcm = new short[n];
    double amp = Math.Pow(10, db / 20) * 32767;
    for (int i = 0; i < n; i++) pcm[i] = (short)(amp * Math.Sin(2 * Math.PI * freq * i / rate));
    var format = new WAVEFORMATEX { wFormatTag = 1, nChannels = 1, nSamplesPerSec = (uint)rate, nAvgBytesPerSec = (uint)rate * 2, nBlockAlign = 2, wBitsPerSample = 16, cbSize = 0 };
    IntPtr h; uint r = waveOutOpen(out h, new IntPtr(device), ref format, IntPtr.Zero, IntPtr.Zero, 0);
    if (r != 0) return "waveOutOpen " + r;
    var data = Marshal.AllocHGlobal(n * 2); Marshal.Copy(pcm, 0, data, n);
    var header = new WAVEHDR { lpData = data, dwBufferLength = (uint)(n * 2) };
    var hdr = Marshal.AllocHGlobal(Marshal.SizeOf(header)); Marshal.StructureToPtr(header, hdr, false);
    uint size = (uint)Marshal.SizeOf(header);
    waveOutPrepareHeader(h, hdr, size);
    waveOutWrite(h, hdr, size);
    Thread.Sleep((int)(seconds * 1000) + 200);
    waveOutReset(h); waveOutUnprepareHeader(h, hdr, size); waveOutClose(h);
    Marshal.FreeHGlobal(data); Marshal.FreeHGlobal(hdr);
    return "played on device " + device;
  }
}
"@
[CableTone]::Play($Seconds, $Freq, $AmplitudeDb)
