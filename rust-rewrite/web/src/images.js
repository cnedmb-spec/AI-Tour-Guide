// 图片托盘：拖进窗口或点「相机」选图，压缩后随问题一起提交。
// 不落盘、不上传服务器磁盘——压缩得到的 base64 直接放进 /api/ask 的 images 字段。
const MAX_SHOTS = 3;
const MAX_EDGE = 768;      // 长边 768，视觉 token 少，8GB 显存才稳
const QUALITY = 0.72;

function loadImage(file) {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => { URL.revokeObjectURL(url); resolve(img); };
    img.onerror = () => { URL.revokeObjectURL(url); reject(new Error('图片读取失败')); };
    img.src = url;
  });
}

async function compress(file) {
  const img = await loadImage(file);
  const scale = Math.min(1, MAX_EDGE / Math.max(img.naturalWidth, img.naturalHeight));
  const w = Math.max(1, Math.round(img.naturalWidth * scale));
  const h = Math.max(1, Math.round(img.naturalHeight * scale));
  const canvas = document.createElement('canvas');
  canvas.width = w;
  canvas.height = h;
  canvas.getContext('2d').drawImage(img, 0, 0, w, h);
  const blob = await new Promise((r) => canvas.toBlob(r, 'image/jpeg', QUALITY));
  const dataUrl = await new Promise((r) => {
    const fr = new FileReader();
    fr.onload = () => r(fr.result);
    fr.readAsDataURL(blob);
  });
  return { dataUrl, base64: String(dataUrl).split(',')[1] || '' };
}

export function createShotTray({ max = MAX_SHOTS, onChange } = {}) {
  const tray = document.getElementById('shots');
  const hint = document.getElementById('dropHint');
  const picker = document.getElementById('shotPick');
  const items = [];
  let dragDepth = 0;

  const notify = () => onChange && onChange(items.length);

  function render() {
    tray.innerHTML = '';
    tray.classList.toggle('on', items.length > 0);
    items.forEach((it, i) => {
      const cell = document.createElement('div');
      cell.className = 'shot';
      const img = document.createElement('img');
      img.src = it.dataUrl;
      img.alt = `待识别图片 ${i + 1}`;
      const x = document.createElement('button');
      x.type = 'button';
      x.className = 'x';
      x.textContent = '×';
      x.setAttribute('aria-label', `移除第 ${i + 1} 张图`);
      x.onclick = () => { items.splice(i, 1); render(); notify(); };
      cell.append(img, x);
      tray.append(cell);
    });
  }

  // 并发调用串行化：选图与拖拽同时发生时，「上限检查 → await 压缩」之间存在竞态，
  // 可能超过 max 张（DEF-28）。这里用一个队列把所有待处理文件串起来。
  let adding = false;
  const queue = [];
  async function addFiles(files) {
    const picked = Array.from(files || []).filter((f) => f.type.startsWith('image/'));
    queue.push(...picked);
    if (adding) return;
    adding = true;
    let dropped = 0;
    let failed = 0;
    try {
      while (queue.length) {
        const f = queue.shift();
        // 超出上限的张数要如实告知，不能静默丢图
        if (items.length >= max) { dropped += 1; continue; }
        try {
          items.push(await compress(f));
          render();
          notify();
        } catch (e) {
          failed += 1;
          console.warn('[shot] 压缩失败', e);
        }
      }
    } finally {
      adding = false;
    }
    if (dropped || failed) {
      window.dispatchEvent(new CustomEvent('shot-limit', { detail: { max, dropped, failed } }));
    }
  }

  // 拖拽：必须 preventDefault，否则浏览器会直接打开图片
  window.addEventListener('dragenter', (e) => {
    if (!e.dataTransfer || !Array.from(e.dataTransfer.types || []).includes('Files')) return;
    e.preventDefault();
    dragDepth += 1;
    if (hint) hint.classList.add('on');
  });
  window.addEventListener('dragover', (e) => {
    if (!e.dataTransfer) return;
    // 任何拖拽都拦下：拖文本/链接进窗口时浏览器会直接导航离开页面（DEF-28）
    e.preventDefault();
    if (Array.from(e.dataTransfer.types || []).includes('Files')) e.dataTransfer.dropEffect = 'copy';
  });
  window.addEventListener('dragleave', (e) => {
    dragDepth = Math.max(0, dragDepth - 1);
    if (!dragDepth && hint) hint.classList.remove('on');
  });
  window.addEventListener('drop', (e) => {
    if (!e.dataTransfer) return;
    e.preventDefault();
    dragDepth = 0;
    if (hint) hint.classList.remove('on');
    addFiles(e.dataTransfer.files);
  });
  if (picker) {
    picker.onchange = () => {
      addFiles(picker.files);
      picker.value = '';
    };
  }

  return {
    addFiles,
    count: () => items.length,
    has: () => items.length > 0,
    take() {
      const out = items.map((it) => it.base64).filter(Boolean);
      items.length = 0;
      render();
      notify();
      return out;
    },
    clear() { items.length = 0; render(); notify(); },
  };
}
