// 音频：播放与口型同步 / 麦克风采集与 16k 重采样
export class Speaker {
  constructor() {
    this.ctx = new AudioContext({ latencyHint: 'interactive' });
    this.sources = new Set();    // 所有在播的声音，切换问答时统一掐掉
    this._finishers = new Map(); // src -> finish()：stop 时主动结束 Promise（SUS-01）
  }

  async unlock() {
    if (this.ctx.state === 'suspended') await this.ctx.resume();
  }

  async play(audio, visemes, onViseme) {
    // 兼容两种入参：base64 字符串（短音频内联）或 ArrayBuffer（长音频走 /api/audio 拉取）
    let bytes;
    if (audio instanceof ArrayBuffer) {
      bytes = new Uint8Array(audio);
    } else {
      const bin = atob(String(audio || ''));
      bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    }
    const audioBuf = await this.ctx.decodeAudioData(bytes.buffer);

    const src = this.ctx.createBufferSource();
    src.buffer = audioBuf;
    const analyser = this.ctx.createAnalyser();
    analyser.fftSize = 512;
    src.connect(analyser);
    analyser.connect(this.ctx.destination);

    const data = new Uint8Array(analyser.fftSize);
    const startAt = this.ctx.currentTime + 0.06;
    src.start(startAt);
    this.sources.add(src);

    let raf = 0;
    let done = false;
    let resolveFn = null;
    const finished = new Promise((resolve) => { resolveFn = resolve; });
    const finish = () => {
      if (done) return;
      done = true;
      cancelAnimationFrame(raf);
      this.sources.delete(src);
      this._finishers.delete(src);
      try { src.disconnect(); analyser.disconnect(); } catch (e) { /* 已断开 */ }
      onViseme(null, 0);
      resolveFn();
    };
    this._finishers.set(src, finish);
    src.onended = finish;

    const tick = () => {
      if (done) return;
      const t = this.ctx.currentTime - startAt;
      if (t >= 0 && t <= audioBuf.duration) {
        analyser.getByteTimeDomainData(data);
        let sum = 0;
        for (let i = 0; i < data.length; i++) {
          const v = (data[i] - 128) / 128;
          sum += v * v;
        }
        const rms = Math.sqrt(sum / data.length);
        const amp = Math.min(1, rms * 4.2);
        let cur = visemes[0] ? visemes[0].v : 'aa';
        for (const s of visemes) {
          if (t >= s.t && t < s.t + s.d) { cur = s.v; break; }
        }
        onViseme(cur, amp * 0.85);
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);

    return finished;
  }

  stop() {
    for (const s of Array.from(this.sources)) {
      try { s.stop(); } catch (e) { /* 已结束 */ }
      // 主动结束等待中的 play()：个别内核在 start 前停止或后台挂起时不派发 ended，
      // 否则 ask() 会永远 await 在播放上，界面卡在「讲解中」
      const f = this._finishers.get(s);
      if (f) f();
    }
    this.sources.clear();
    this._finishers.clear();
  }
}

// 录音用 AudioContext 跨会话复用：iOS Safari 对并发 AudioContext 数量敏感，
// 每次「按住说话」新建再 close 会很快触顶（DEF-12）
let sharedCtx = null;
async function sharedAudioContext() {
  if (!sharedCtx || sharedCtx.state === 'closed') sharedCtx = new AudioContext();
  if (sharedCtx.state === 'suspended') { try { await sharedCtx.resume(); } catch (e) { /* 忽略 */ } }
  return sharedCtx;
}

export class Mic {
  constructor() {
    this.ctx = null;
    this.stream = null;
    this.node = null;
    this.src = null;
    this.ws = null;
    this.onText = null;
    this.stopped = false;
  }

  async start(onPartial, onFinal, onError) {
    this.onText = { partial: onPartial, final: onFinal };
    this.onError = onError;
    this.stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    this.ctx = await sharedAudioContext();
    const src = this.ctx.createMediaStreamSource(this.stream);
    this.src = src;
    this.node = this.ctx.createScriptProcessor(4096, 1, 1);

    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    this.ws = new WebSocket(`${proto}://${location.host}/ws/asr`);
    // 语音服务没起来时不能无限等：3.5 s 内连不上就报错，让上层复位
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('ASR_TIMEOUT')), 3500);
      this.ws.onopen = () => { clearTimeout(timer); resolve(); };
      this.ws.onerror = () => { clearTimeout(timer); reject(new Error('ASR_UNREACHABLE')); };
      this.ws.onclose = () => {
        if (this.ws && this.ws.readyState !== WebSocket.OPEN) {
          clearTimeout(timer);
          reject(new Error('ASR_CLOSED'));
        }
      };
    });
    // 连接建立后重绑 onclose：服务端中途断开（如录音超上限、服务重启）要通知上层，
    // 而不是静默卡在「聆听中」（DEF-19）
    this.ws.onclose = () => { if (!this.stopped && this.onError) this.onError('语音服务连接已断开'); };
    this.ws.onmessage = (ev) => {
      const d = JSON.parse(ev.data);
      if (d.type === 'partial') this.onText.partial(d.text);
      if (d.type === 'final') this.onText.final(d.text);
      if (d.type === 'error' && this.onError) this.onError(d.text);
    };
    this.ws.onerror = () => { if (this.onError) this.onError('语音服务连接失败'); };

    const ratio = this.ctx.sampleRate / 16000;
    this.node.onaudioprocess = (e) => {
      const input = e.inputBuffer.getChannelData(0);
      const outLen = Math.floor(input.length / ratio);
      const out = new Float32Array(outLen);
      for (let i = 0; i < outLen; i++) {
        let acc = 0;
        const s = Math.floor(i * ratio);
        const e2 = Math.floor((i + 1) * ratio);
        for (let j = s; j < e2; j++) acc += input[j];
        out[i] = acc / Math.max(1, e2 - s);
      }
      if (this.ws && this.ws.readyState === 1) this.ws.send(out);
    };

    // ScriptProcessor 必须接出去才会被驱动；用 0 增益接到 destination，避免自己的声音被放出来
    const mute = this.ctx.createGain();
    mute.gain.value = 0;
    src.connect(this.node);
    this.node.connect(mute);
    mute.connect(this.ctx.destination);
    this.mute = mute;
  }

  async flush() {
    if (!this.ws || this.ws.readyState !== 1) return;
    // 文本帧 "END" 告诉服务端：录音结束，用离线模型重跑整段，出最终结果
    this.ws.send('END');
  }

  async stop() {
    this.stopped = true;
    if (this.node) { this.node.disconnect(); this.node.onaudioprocess = null; }
    if (this.mute) { try { this.mute.disconnect(); } catch (e) { /* 忽略 */ } }
    if (this.src) { try { this.src.disconnect(); } catch (e) { /* 忽略 */ } }
    // AudioContext 跨录音复用（iOS 对并发 AudioContext 数量敏感），不再每次 close（DEF-12）
    if (this.stream) this.stream.getTracks().forEach((t) => t.stop());
    if (this.ws) { try { this.ws.close(); } catch (e) { /* 忽略 */ } }
    this.node = this.src = this.ctx = this.stream = this.ws = this.mute = null;
  }
}
