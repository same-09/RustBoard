# Loopback patch for VirtualDrivers/Virtual-Audio-Driver (MIT)

Upstream: speaker discards audio, mic sends silence. This patch connects them:
audio played to "Virtual Audio Driver" comes out of "Virtual Mic Driver".

Changed file: Source/Main/minwavertstream.cpp (only this one).
Apply: `git apply loopback.patch` or replace the file.

## Limits
- Speaker and mic must use the SAME format. Use 48 kHz, 16-bit, 2 channels on both
  (Sound settings -> device -> Properties -> Advanced). Different format = mic is silent.
- Latency target about 20 ms, hard cap 60 ms.
- NOT COMPILED, NOT TESTED. Kernel code: test in a VM first, a bug can blue-screen Windows.

## Build and install
1. Install Visual Studio 2022 + matching WDK.
2. Run build.bat (or open VirtualAudioDriver.sln, Release x64).
3. Test-sign: `bcdedit /set testsigning on`, reboot.
4. Device Manager -> Add Legacy Hardware -> Have Disk -> VirtualAudioDriver.inf.

## Use with Rustboard
- Rustboard Secondary output (and Mic output) = Virtual Audio Driver (speaker)
- Discord Input Device = Virtual Mic Driver
- Discord: turn off Noise Suppression, Echo Cancellation, Auto Gain.
