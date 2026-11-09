// 页面交互层：导览册、快捷提问、问答记录、字幕、错误恢复、演示模式、自检、评审资料
const $ = (id) => document.getElementById(id);

export function createUI({ theme, themeList, guide, handlers }) {
  const info = (guide && Array.isArray(guide.exhibits))
    ? guide
    : { exhibits: theme.exhibits || [], chitchat: [], faq: [] };
  const ui = { state: 'idle', history: [] };
  // 页面刷新（比如 WebGL 恢复后自动 reload）不该把问答记录清空
  try {
    const saved = JSON.parse(sessionStorage.getItem('qx_history') || '[]');
    if (Array.isArray(saved)) ui.history = saved;
  } catch (e) { /* 缓存损坏就当没有 */ }

  // ─────────── 导览册 ───────────
  function renderExhibits() {
    const box = $('exhibitList');
    box.innerHTML = '';
    info.exhibits.forEach((ex) => {
      const b = document.createElement('button');
      b.className = 'exhibit';
      b.type = 'button';
      // 主题包内容按纯文本插入，杜绝 innerHTML 注入
      const n = document.createElement('div');
      n.className = 'n';
      n.textContent = ex.name || '';
      const brief = document.createElement('div');
      brief.className = 'b';
      brief.textContent = ex.brief || '';
      b.append(n, brief);
      b.onclick = () => handlers.ask(`介绍一下${ex.name || ''}`);
      box.appendChild(b);
    });
    $('guideSub').textContent = `${info.exhibits.length} 个展项 · 结构化知识库`;
    if ($('introText')) {
      $('introText').textContent =
        `讲解员「${theme.assistant.name}」站在展厅里。可以按住空格说话，也可以直接打字；` +
        `问到哪个展项，她会转身走过去讲，字幕跟着声音走。断网也能用。`;
    }
  }

  // 导览册里的示例问题：点一下先填进输入框，可改再发
  function renderExamples() {
    const box = $('exampleList');
    box.innerHTML = '';
    const pool = [];
    info.exhibits.forEach((ex) => {
      if (ex.faq && ex.faq[0]) pool.push(ex.faq[0]);
    });
    (info.chitchat || []).slice(0, 2).forEach((q) => pool.push(q));
    pool.slice(0, 8).forEach((q) => {
      const b = document.createElement('button');
      b.className = 'chip';
      b.textContent = q;
      b.onclick = () => {
        const el = $('textInput');
        el.value = q;
        el.focus();
        toast('已填入输入框，可按回车发送');
      };
      box.appendChild(b);
    });
  }

  // 首屏字幕区上方的快捷提问：一键直接问（演示用）
  function renderQuickAsk() {
    const box = $('quickAsk');
    if (!box) return;
    box.innerHTML = '';
    const pool = [];
    info.exhibits.forEach((ex) => { if (ex.faq && ex.faq[0]) pool.push(ex.faq[0]); });
    pool.slice(0, 3).forEach((q) => {
      const b = document.createElement('button');
      b.className = 'qchip';
      b.textContent = q;
      b.onclick = () => handlers.ask(q);
      box.appendChild(b);
    });
  }

  function renderThemes() {
    const box = $('themes');
    box.innerHTML = '';
    if (!themeList || themeList.length < 2) return;
    themeList.forEach((t) => {
      const b = document.createElement('button');
      b.className = 'theme-btn' + (t.id === theme.id ? ' on' : '');
      b.textContent = t.short;
      b.style.cssText = 'padding:6px 12px;font-size:10.5px;letter-spacing:.12em;border:1px solid var(--line);' +
        'background:rgba(255,255,255,.02);border-radius:2px;color:var(--dim)';
      b.onclick = async () => {
        try {
          const r = await fetch(`/api/theme/${encodeURIComponent(t.id)}`, { method: 'POST' });
          if (r.ok) {
            // 通知其他标签页：storage 事件触发它们自动刷新对齐新主题（DEF-08）
            try { localStorage.setItem('qx_theme_ver', String(Date.now())); } catch (e2) { /* 忽略 */ }
            location.reload();
            return;
          }
          toast('主题切换失败，请重试', 'error');
        } catch (e) {
          toast('主题切换失败：本地服务无响应', 'error');
        }
      };
      box.appendChild(b);
    });
  }

  // ─────────── 状态 / 字幕 ───────────
  function setStatus(text, kind = 'idle') {
    ui.state = kind;
    $('statusText').textContent = text;
    $('dot').className = 'dot' + (kind === 'idle' ? '' : ' ' + kind);
  }

  let typeTimer = null;
  let subtitleText = '';
  function showSubtitle(text, who = theme.assistant.name, duration = 0) {
    if (!text) return;
    subtitleText = text;
    const el = $('subtitle');
    el.innerHTML = `<span class="who"></span><span class="line"></span><span class="cursor"></span>`;
    el.querySelector('.who').textContent = who || '';
    el.classList.add('show');
    const line = el.querySelector('.line');
    const cur = el.querySelector('.cursor');
    // 后端 duration 由 wav 帧数算出，与播放侧实际音频长度可能有偏差；
    // 逐字速度按整段时长铺满，语音真正结束时再由 finishSubtitle() 补齐残句。
    const total = Math.max(900, (duration || 6) * 1000);
    const step = total / Math.max(1, text.length);
    let i = 0;
    clearInterval(typeTimer);
    typeTimer = setInterval(() => {
      line.textContent = text.slice(0, ++i);
      if (i >= text.length) {
        clearInterval(typeTimer);
        cur.remove();
      }
    }, step);
  }
  // 音频播完（或合成失败）时把字幕补成全文，避免「字已打完还在讲」
  // 或「讲完了字还没打完」；被打断时 clearSubtitle 已清空 subtitleText。
  function finishSubtitle() {
    if (!subtitleText) return;
    clearInterval(typeTimer);
    const el = $('subtitle');
    const line = el.querySelector('.line');
    const cur = el.querySelector('.cursor');
    if (line) line.textContent = subtitleText;
    if (cur) cur.remove();
  }
  function clearSubtitle() {
    subtitleText = '';
    clearInterval(typeTimer);
    const el = $('subtitle');
    el.classList.remove('show');
    el.innerHTML = '';
  }
  function partial(text) {
    if (!text) return;
    clearInterval(typeTimer);
    // 识别文本来自麦克风，属于不可信输入：用 textContent 组装，不拼 innerHTML
    const el = $('subtitle');
    const who = document.createElement('span');
    const line = document.createElement('span');
    who.className = 'who';
    who.textContent = '正在识别';
    line.className = 'line';
    line.textContent = text;
    el.replaceChildren(who, line);
    el.classList.add('show');
  }

  // ─────────── 问答记录 ───────────
  function persistHistory() {
    try {
      sessionStorage.setItem('qx_history', JSON.stringify(ui.history.slice(-60)));
    } catch (e) { /* 配额满了就只留内存里的 */ }
  }
  function pushQA({ q, a, exhibit, elapsed }) {
    const t = new Date();
    ui.history.push({
      q, a, exhibit,
      time: `${String(t.getHours()).padStart(2, '0')}:${String(t.getMinutes()).padStart(2, '0')}`,
      elapsed,
    });
    persistHistory();
    renderHistory();
  }
  // 连问时一帧只重绘一次：25 轮压力下不再反复重建整块列表
  let historyPending = false;
  function renderHistory() {
    if (historyPending) return;
    historyPending = true;
    requestAnimationFrame(() => {
      historyPending = false;
      renderHistoryNow();
    });
  }
  function renderHistoryNow() {
    const box = $('historyBody');
    const cancelled = ui.history.reduce((n, h) => n + (h.cancelled ? h.folded || 1 : 0), 0);
    const valid = ui.history.filter((h) => !h.cancelled).length;
    // 计数口径与文案对齐（DEF-26）：总数 = 有效 + 已取消
    $('historyCount').textContent = cancelled ? `${valid + cancelled} 条（其中 ${cancelled} 条已取消）` : `${valid} 条`;
    if (!ui.history.length) {
      box.innerHTML = '<div class="empty">还没有对话。<br>问一句「介绍一下碉楼」试试。</div>';
      return;
    }
    box.innerHTML = ui.history
      .slice()
      .reverse()
      .map((h) => `<div class="qa${h.cancelled ? ' cancelled' : ''}">
        <div class="q">Q · ${escapeHtml(h.q)}</div>
        <div class="a">${escapeHtml(h.a)}</div>
        <div class="meta">${escapeHtml(h.time || '')}${h.exhibit ? ' · ' + escapeHtml(h.exhibit) : ''}${h.elapsed ? ' · ' + escapeHtml(String(h.elapsed)) + ' ms' : ''}</div>
      </div>`)
      .join('');
  }
  function clearHistory() {
    ui.history = [];
    try { sessionStorage.removeItem('qx_history'); } catch (e) { /* 忽略 */ }
    renderHistory();
    clearSubtitle();
  }

  // 被新提问顶掉的上一问，记一条“已取消”，不让它凭空消失
  function pushCancelled(q) {
    const t = new Date();
    const time = `${String(t.getHours()).padStart(2, '0')}:${String(t.getMinutes()).padStart(2, '0')}`;
    const last = ui.history[ui.history.length - 1];
    // 连续被打断时折叠成一条，避免连点压力下堆成二十多条
    if (last && last.cancelled) {
      last.folded = (last.folded || 1) + 1;
      last.a = `（已取消 ${last.folded} 题：新提问连续打断）`;
      last.time = time;
      persistHistory();
      renderHistory();
      return;
    }
    ui.history.push({
      q,
      a: '（已取消：新提问打断了这一题）',
      exhibit: '',
      time,
      cancelled: true,
    });
    persistHistory();
    renderHistory();
  }

  function escapeHtml(s) {
    return String(s).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
  }

  // ─────────── 指标 ───────────
  function setElapsed(ms) {
    // 同一句话第二次问会命中语音缓存，几十毫秒就出声；如实标注，别让评委以为是造假
    const cached = ms < 200;
    $('sResp').textContent = ms + ' ms';
    $('mResp').textContent = ms + ' ms';
    const note = cached ? '端到端响应 · 命中缓存' : '端到端响应（提问→出声）';
    $('sRespNote').textContent = note;
    $('mRespNote').textContent = note;
  }
  function resetMetrics() {
    $('sResp').textContent = '—';
    $('mResp').textContent = '—';
    $('sRespNote').textContent = '端到端响应（首问后显示）';
    $('mRespNote').textContent = '端到端响应（提问→出声）';
  }

  async function loadStats() {
    // abort 定时器在 try 外声明，异常路径也能清理（OPT-10）
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), 6000);
    try {
      const r = await fetch('/api/stats', { cache: 'no-store', signal: ctrl.signal });
      clearTimeout(t);
      if (!r.ok) return null;
      const s = await r.json();
      $('sKb').textContent = s.kb_answers;
      $('mKb').textContent = s.kb_answers + ' 条';
      $('sTts').textContent = s.tts_rtf ? Number(s.tts_rtf).toFixed(2) : '—';
      $('mAsr').textContent = s.asr_load_ms ? (Number(s.asr_load_ms) / 1000).toFixed(1) + ' s' : '加载中';
      $('mTts').textContent = s.tts_load_ms ? (Number(s.tts_load_ms) / 1000).toFixed(1) + ' s' : '加载中';
      $('mLlm').textContent = s.llm && s.llm.available
        ? (s.llm.model || '本地千问') + (s.llm_load_ms ? ' · ' + (Number(s.llm_load_ms) / 1000).toFixed(1) + ' s' : '')
        : '未连接';
      // 技术架构弹层里的模型名跟随真实配置，不再写死 qwen2.5:7b（DEF-11）
      const tech = $('mLlmTech');
      if (tech) {
        tech.textContent = s.llm && s.llm.available
          ? `本地千问 ${s.llm.model || ''}`.trim()
          : '本地千问（未连接）';
      }
      return s;
    } catch (e) {
      clearTimeout(t);
      return null;
    }
  }

  // ─────────── 弹层 ───────────
  let lastFocus = null;   // 打开弹层前的焦点，关闭时归还
  function openSheet(kind) {
    $('techPane').style.display = kind === 'tech' ? 'block' : 'none';
    $('reviewPane').style.display = kind === 'review' ? 'block' : 'none';
    $('checkPane').style.display = kind === 'check' ? 'block' : 'none';
    $('sheetTitle').textContent = kind === 'tech' ? '技术架构' : kind === 'review' ? '评审资料' : '演示前自检';
    if (!$('modal').classList.contains('show')) {
      lastFocus = document.activeElement;
      $('modal').classList.add('show');
      try { $('sheetClose').focus({ preventScroll: true }); } catch (e) { $('sheetClose').focus(); }
    }
    // 「导览册」高亮跟面板走，其余按钮跟弹层走，关闭后不再互相错乱；
    // 「演示模式」按钮独立管理，弹层开关不能抹掉它的激活态（DEF-16）
    document.querySelectorAll('.nav button').forEach((b) => {
      if (b.dataset.act === 'demo') return;
      if (b.dataset.act === 'guide') {
        b.classList.toggle('on', !$('guidePanel').classList.contains('hide'));
      } else {
        b.classList.toggle('on', b.dataset.act === (kind === 'check' ? 'selfcheck' : kind));
      }
    });
    if (kind === 'check') runSelfCheck();
  }
  function closeSheet() {
    if (!$('modal').classList.contains('show')) return;
    $('modal').classList.remove('show');
    // 「演示模式」按钮独立管理，这里不碰它的激活态（DEF-16）
    document.querySelectorAll('.nav button').forEach((b) => {
      if (b.dataset.act === 'demo') return;
      if (b.dataset.act !== 'guide') b.classList.remove('on');
    });
    const gbtn = document.querySelector('.nav button[data-act="guide"]');
    if (gbtn) gbtn.classList.toggle('on', !$('guidePanel').classList.contains('hide'));
    // 焦点归还给打开弹层前的元素
    if (lastFocus && lastFocus.focus) {
      try { lastFocus.focus({ preventScroll: true }); } catch (e) { lastFocus.focus(); }
    }
    lastFocus = null;
  }

  // ─────────── 自检 ───────────
  const checks = [];
  let selfCheckBusy = false;   // 自检进行中标记，防连点重入
  function addCheck(name, val) { checks.push({ name, ...val }); }

  async function runSelfCheck() {
    // 防重入：连点会让共享的 checks 数组互相覆盖，结果重复或缺失
    if (selfCheckBusy) return;
    selfCheckBusy = true;
    const rebtn = $('btnRecheck');
    if (rebtn) rebtn.disabled = true;
    try {
      await runSelfCheckNow();
    } finally {
      selfCheckBusy = false;
      if (rebtn) rebtn.disabled = false;
    }
  }
  async function runSelfCheckNow() {
    checks.length = 0;
    const box = $('checkList');
    box.innerHTML = '<div class="checkrow"><span class="flag wait">检测中</span><span class="name">正在逐项检查…</span></div>';

    // 1. WebGL
    let gl = null;
    try {
      const c = document.createElement('canvas');
      gl = c.getContext('webgl2') || c.getContext('webgl');
    } catch (e) { /* 忽略 */ }
    addCheck('3D 渲染（WebGL）', gl
      ? { ok: true, val: (gl.getParameter(gl.VERSION) || '').slice(0, 26) }
      : { ok: false, val: '不可用，已切降级模式' });

    // 2. 场景与模型
    const w = window.world;
    const nModel = w && w.pedestals ? Object.values(w.pedestals).filter((p) => p.prop).length : 0;
    addCheck('3D 场景与展品', nModel > 0
      ? { ok: true, val: `${nModel} 个展台已就位` }
      : { ok: false, val: '场景未加载（已降级，不影响问答）' });

    // 3. 本地服务
    const s = await loadStats();
    addCheck('本地服务接口', s
      ? { ok: true, val: `主题 ${s.theme} · 请求 ${s.requests} 次` }
      : { ok: false, val: '接口无响应' });

    // 4. 语音链路
    if (s) {
      addCheck('离线语音识别', s.asr_load_ms > 0
        ? { ok: true, val: `模型已加载 ${(s.asr_load_ms / 1000).toFixed(1)} s` }
        : { ok: false, val: '模型仍在加载或未启动' });
      addCheck('离线语音合成', s.tts_load_ms > 0
        ? { ok: true, val: `已就绪 ${(s.tts_load_ms / 1000).toFixed(1)} s` }
        : { ok: false, val: '未就绪' });
      addCheck('本地大模型', s.llm && s.llm.available
        ? { ok: true, val: s.llm.model || '本地千问' }
        : { ok: false, val: '未连接 Ollama，将只用知识库作答' });
    }

    // 5. 安全上下文 + 麦克风权限
    addCheck('安全上下文', {
      ok: !!window.isSecureContext,
      val: window.isSecureContext ? `localhost / HTTPS（${location.host}）` : `非安全上下文（${location.host}），麦克风不可用`,
    });
    let micVal = '未知（首次按住说话时申请）';
    let micOk = true;
    try {
      if (navigator.permissions && navigator.permissions.query) {
        const st = await navigator.permissions.query({ name: 'microphone' });
        micVal = st.state === 'granted' ? '已授权' : st.state === 'denied' ? '被拒绝，请改用文字输入' : '未授权（首次会弹窗）';
        micOk = st.state !== 'denied';
      }
    } catch (e) { micVal = '未知（浏览器不支持权限查询，首次按住说话时申请）'; }
    addCheck('麦克风权限', { ok: micOk, val: micVal });

    // 6. 显示与音频设备
    addCheck('显示与缩放', {
      ok: window.innerWidth >= 1280 && window.innerHeight >= 700,
      val: `${window.innerWidth}×${window.innerHeight} · 系统 ${screen.width}×${screen.height} · 缩放 ${Math.round(devicePixelRatio * 100)}%`,
    });
    try {
      const devs = await navigator.mediaDevices.enumerateDevices();
      const micN = devs.filter((d) => d.kind === 'audioinput').length;
      const spkN = devs.filter((d) => d.kind === 'audiooutput').length;
      addCheck('音频设备', { ok: micN > 0, val: `麦克风 ${micN} 个 · 扬声器 ${spkN ? spkN + ' 个' : '默认'}` });
    } catch (e) {
      addCheck('音频设备', { ok: true, val: '浏览器不支持枚举，按默认设备输出' });
    }

    // 7. 资源本地化
    const entries = performance.getEntriesByType('resource');
    const remote = entries.filter((e) => /^https?:/.test(e.name) && !e.name.startsWith(location.origin));
    addCheck('资源本地化', remote.length === 0
      ? { ok: true, val: `${entries.length} 个资源全部同源` }
      : { ok: false, val: `${remote.length} 个外部资源` });

    // 8. 离线可用性：探一下本地 vendor 是否可达（HEAD 不传大文件体），不靠 navigator.onLine 猜
    let offOk = false;
    let offVal = '本地资源读取失败';
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), 6000);
    try {
      const t0 = performance.now();
      const r = await fetch('/vendor/three.module.js', { method: 'HEAD', cache: 'no-store', signal: ctrl.signal });
      clearTimeout(timer);
      offOk = r.ok;
      offVal = offOk ? `本地 vendor 可达 ${Math.round(performance.now() - t0)} ms` : `HTTP ${r.status}`;
    } catch (e) {
      clearTimeout(timer);
      offVal = '本地资源读取失败或超时';
    }
    if (!navigator.onLine) offVal += ' · 当前离线';
    addCheck('离线可用性', { ok: offOk, val: offVal });

    box.innerHTML = checks.map((c) => `<div class="checkrow">
        <span class="flag ${c.ok ? 'ok' : 'bad'}">${c.ok ? '通过' : '注意'}</span>
        <span class="name">${escapeHtml(c.name)}</span>
        <span class="val">${escapeHtml(c.val || '')}</span>
      </div>`).join('');
  }

  // ─────────── 演示模式 ───────────
  const DEMO = [
    {
      t: '说明问题', sec: 30,
      p: ['侨乡景区讲解人力不足、旺季排队', '讲解内容口径不一，游客听到的信息碎片化', '本作品：可对话、可扩内容、可离线复制的讲解员'],
    },
    {
      t: '操作演示', sec: 60,
      p: [
        '点击左侧「开平碉楼」开讲',
        '按住空格问「为什么叫碉楼」',
        '追问「碉楼是谁盖的」——答：南洋、美洲华侨寄钱寄图纸，本地匠人施工',
        '再问「开平有多少座碉楼」——答：一千八百多座，2007 年列入世界文化遗产',
        '被打断就点「停止播报」，记录里会留一条「已取消」，不装作没发生',
      ],
    },
    {
      t: '技术亮点', sec: 60,
      p: [
        '打开「技术架构」：四层链路一览',
        '常问的题走结构化知识，几十毫秒出声；同一句第二次问命中语音缓存，响应显示 3 ms 就标「命中缓存」',
        '知识库里没有的，才交给本机千问；重要史实不放大模型，避免它编年份、编数字',
      ],
    },
    {
      t: '应用价值', sec: 45,
      p: [
        '旺季讲解员排不过来，一队人挤在碉楼底下听不清',
        '办个临时展，讲解词要重写一遍',
        '游客问「这楼为什么这么高」，讲解员不一定答得上',
        '一台电脑部署，无云服务费；主题包改文件就能换展',
      ],
    },
    {
      t: '团队与应急', sec: 30,
      p: ['说明三人分工：建模、算法、讲解词', '断网/异常时切本地录屏与降级页', '自检面板逐项亮绿灯再开始'],
    },
    {
      t: '合规说明', sec: 15,
      p: [
        '模型自建、素材原创、开源组件清单可查',
        '录音仅经本机回环，不出本机、不落盘',
        '顺带说明：宋体标题、琥珀配色、Maya 建模、缓存命中标注与「已取消」记录，都是开发留下的痕迹',
      ],
    },
  ];
  let demoIdx = -1;
  let demoWasGuideOpen = false;
  let demoTimer = 0;
  let demoLeft = 0;

  function renderDemo() {
    const d = DEMO[demoIdx];
    $('demoIdx').textContent = `STEP ${demoIdx + 1} / ${DEMO.length}`;
    $('demoTitle').textContent = d.t;
    $('demoPoints').innerHTML = d.p.map((x) => `<li>${x}</li>`).join('');
    $('demoBar').innerHTML = DEMO.map((_, i) => `<i class="${i < demoIdx ? 'done' : i === demoIdx ? 'cur' : ''}"></i>`).join('');
    $('demoPrev').disabled = demoIdx === 0;
    $('demoNext').textContent = demoIdx === DEMO.length - 1 ? '结束演示' : '下一步';
    demoLeft = d.sec;
    paintDemoLeft();
    clearInterval(demoTimer);
    demoTimer = setInterval(() => {
      demoLeft = Math.max(0, demoLeft - 1);
      paintDemoLeft();
      if (demoLeft === 0) clearInterval(demoTimer);
    }, 1000);
  }
  function paintDemoLeft() {
    const d = DEMO[demoIdx];
    if (!d) return;
    const over = demoLeft === 0;
    $('demoDur').textContent = over ? `时间到（目标 ${d.sec} 秒）` : `剩余 ${demoLeft} 秒 · 目标 ${d.sec} 秒`;
    $('demoDur').style.color = demoLeft <= 5 ? '#e0897c' : '';
  }
  function startDemo() {
    demoIdx = 0;
    demoWasGuideOpen = !$('guidePanel').classList.contains('hide');
    $('guidePanel').classList.add('hide');
    $('demoBox').classList.add('show');
    renderDemo();
  }
  function demoStep(n) {
    demoIdx = Math.max(0, Math.min(DEMO.length - 1, n));
    renderDemo();
    const step = DEMO[demoIdx];
    if (step.t === '技术亮点') openSheet('tech');
    else if (step.t === '应用价值') { openSheet('review'); switchTab('value'); }
    else closeSheet();
  }
  function endDemo() {
    demoIdx = -1;
    clearInterval(demoTimer);
    $('demoBox').classList.remove('show');
    if (demoWasGuideOpen) $('guidePanel').classList.remove('hide');
    closeSheet();
  }
  function resetDemo() {
    resetAll(true);
    demoStep(0);
  }

  // 一键复位全场：停播报、清记录、清指标、演示回第一步
  function resetAll(silent) {
    handlers.clear();
    clearHistory();
    resetMetrics();
    hideError();
    if ($('demoBox') && $('demoBox').classList.contains('show')) demoStep(0);
    if (!silent) toast('已重置：对话、播报与演示进度');
  }

  function switchTab(name) {
    document.querySelectorAll('#reviewTabs button').forEach((b) => b.classList.toggle('on', b.dataset.tab === name));
    document.querySelectorAll('.pane[data-pane]').forEach((p) => p.classList.toggle('on', p.dataset.pane === name));
  }

  // ─────────── 提示 ───────────
  let toastAt = 0;
  let toastKind = 'info';
  function toast(msg, kind = 'info') {
    const now = Date.now();
    // 错误提示优先：刚弹过错误，2.5 秒内的普通提示不覆盖它
    if (kind !== 'error' && toastKind === 'error' && now - toastAt < 2500) return;
    const t = $('toast');
    t.textContent = msg;
    t.classList.toggle('err', kind === 'error');
    t.classList.add('show');
    toastAt = now;
    toastKind = kind;
    clearTimeout(toast._t);
    toast._t = setTimeout(() => t.classList.remove('show'), kind === 'error' ? 4200 : 2200);
  }

  let retryFn = null;
  function showError(msg, fn) {
    retryFn = fn || null;
    $('errText').textContent = msg;
    $('errRetry').style.display = fn ? '' : 'none';
    $('errbar').classList.add('show');
  }
  function hideError() {
    $('errbar').classList.remove('show');
    $('errText').textContent = '';
  }
  function showBanner(msg) {
    const b = $('banner');
    b.textContent = msg;
    b.classList.add('show');
  }

  // ─────────── 绑定 ───────────
  function bind() {
    renderExhibits();
    renderExamples();
    renderQuickAsk();
    renderThemes();
    renderHistory();
    resetMetrics();

    $('sheetClose').onclick = closeSheet;
    $('modal').addEventListener('click', (e) => { if (e.target === $('modal')) closeSheet(); });
    // 弹层焦点圈：Tab 在弹层内循环，不让焦点穿到背景控件
    $('modal').addEventListener('keydown', (e) => {
      if (e.key !== 'Tab') return;
      const els = Array.from($('modal').querySelectorAll('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'))
        .filter((el) => !el.disabled && el.offsetParent !== null);
      if (!els.length) return;
      const first = els[0];
      const last = els[els.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    });
    $('reviewTabs').onclick = (e) => {
      const b = e.target.closest('button');
      if (b) switchTab(b.dataset.tab);
    };
    $('btnRecheck').onclick = runSelfCheck;

    $('errRetry').onclick = () => { hideError(); if (retryFn) retryFn(); };
    $('errClose').onclick = hideError;

    document.querySelector('.nav').addEventListener('click', (e) => {
      const b = e.target.closest('button');
      if (!b) return;
      const act = b.dataset.act;
      if (act === 'demo') {
        $('demoBox').classList.contains('show') ? endDemo() : startDemo();
        b.classList.toggle('on', $('demoBox').classList.contains('show'));
        return;
      }
      closeSheet();
      if (act === 'guide') {
        $('guidePanel').classList.toggle('hide');
        b.classList.toggle('on', !$('guidePanel').classList.contains('hide'));
        return;
      }
      b.classList.add('on');
      openSheet(act === 'selfcheck' ? 'check' : act);
    });

    $('btnHistory').onclick = () => {
      $('historyPanel').classList.toggle('hide');
      $('btnHistory').classList.toggle('on', !$('historyPanel').classList.contains('hide'));
    };
    $('demoPrev').onclick = () => demoStep(demoIdx - 1);    $('demoNext').onclick = () => (demoIdx >= DEMO.length - 1 ? endDemo() : demoStep(demoIdx + 1));
    $('demoReset').onclick = resetDemo;

    $('btnStop').onclick = () => handlers.stop();
    $('btnReplay').onclick = () => handlers.replay();
    $('btnClear').onclick = () => resetAll(false);
    // R 键随时复位，不一定要先开演示模式；弹层打开时不抢（避免翻资料时误清全场）
    window.addEventListener('keydown', (e) => {
      const typing = document.activeElement && ['INPUT', 'TEXTAREA'].includes(document.activeElement.tagName);
      const modalOpen = $('modal').classList.contains('show');
      if ((e.key === 'r' || e.key === 'R') && !typing && !modalOpen && !e.ctrlKey && !e.metaKey) resetAll(false);
    });
    $('btnSend').onclick = () => {
      const v = $('textInput').value.trim();
      // 只发图不发字也允许：问“这是什么”就够
      if (!v && !handlers.hasImages()) { toast('先输入问题，或拖一张图进来'); return; }
      // ask 同步返回是否受理：只有被受理才清空输入框，防连点吞掉时文字不丢
      if (handlers.ask(v)) $('textInput').value = '';
    };
    $('textInput').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        // 与发送按钮同一判定：生成中按 Enter 不提交，文字留在框里，答案一到再按
        if (ui.busy) return;
        const v = e.target.value.trim();
        if (!v && !handlers.hasImages()) { toast('先输入问题，或拖一张图进来'); return; }
        if (handlers.ask(v)) e.target.value = '';
      }
    });
    const shotBtn = $('shotBtn');
    if (shotBtn) shotBtn.onclick = () => $('shotPick').click();
    window.addEventListener('shot-limit', (e) => {
      const d = (e && e.detail) || {};
      if (d.failed) toast(`有 ${d.failed} 张图读取/压缩失败，已跳过`, 'error');
      if (d.dropped) toast(`最多 ${d.max || 3} 张图，已忽略多出的 ${d.dropped} 张`, 'error');
    });

    // 停止只在播报中可用，重播只要有过回答就能用
    ui.setPlaying = (on) => { $('btnStop').disabled = !on; };
    ui.setReplayable = (on) => { $('btnReplay').disabled = !on; };
    // 回答生成中：快捷提问与发送置灰，避免连点产生一堆“已取消”
    ui.setBusy = (on) => {
      ui.busy = !!on;
      document.querySelectorAll('.qchip').forEach((b) => {
        b.disabled = !!on;
        b.style.opacity = on ? '.42' : '';
        b.style.pointerEvents = on ? 'none' : '';
      });
      const send = $('btnSend');
      if (send) {
        send.disabled = !!on;
        send.classList.toggle('busy', !!on);
      }
      // 生成中也允许点「停止播报」：配合 stopAll() 取消在途请求（DEF-06）
      const stopBtn = $('btnStop');
      if (stopBtn) stopBtn.disabled = !on;
    };
  }

  bind();
  return {
    ...ui,
    setStatus, showSubtitle, finishSubtitle, clearSubtitle, partial,
    pushQA, pushCancelled, clearHistory, renderHistory,
    setElapsed, resetMetrics, loadStats,
    openSheet, closeSheet, toast, showError, hideError, showBanner,
    runSelfCheck, startDemo, endDemo,
  };
}
