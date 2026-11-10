// 人机互动环节（现场问答）：数字人出题 → 观众点选 → 即时判分 → 追问讲解。
//
// 设计上刻意「零后端改动、零新增音频」：每题的 followup 都是主题包里已经存在的
// FAQ 问题，交给现有 /api/ask 命中结构化答案，播放赛前预生成的标准普通话语音。
// 因此字幕、口型、镜头、手势、问答记录全部复用现有链路，也不影响原有讲解流程。
const $ = (id) => document.getElementById(id);

export function createQuiz({ theme, handlers, toast }) {
  const card = $('quizCard');
  const entry = $('btnQuiz');
  if (!card || !entry) return { available: false, count: 0 };

  const raw = (theme && Array.isArray(theme.quiz)) ? theme.quiz : [];
  const bank = raw.filter((q) =>
    q && typeof q.q === 'string'
    && Array.isArray(q.options) && q.options.length >= 2
    && Number.isInteger(q.answer) && q.answer >= 0 && q.answer < q.options.length
    && typeof q.followup === 'string' && q.followup.trim()
  );
  // 主题包没有题库时把入口藏起来，绝不留下点了没反应的按钮
  if (!bank.length) {
    entry.style.display = 'none';
    return { available: false, count: 0 };
  }

  const label = $('quizLabel');
  const title = $('quizTitle');
  const optsBox = $('quizOptions');
  const feedback = $('quizFeedback');
  const nextBtn = $('quizNext');
  const explainBtn = $('quizExplain');
  const closeBtn = $('quizClose');

  let idx = 0;
  let answered = false;

  function render() {
    answered = false;
    const q = bank[idx];
    if (label) label.textContent = `第 ${idx + 1} / ${bank.length} 题 · ${q.label || '侨乡江门'}`;
    if (title) title.textContent = q.q;
    if (feedback) {
      feedback.textContent = '请观众选一个答案，答完小侨会接着把来龙去脉讲给你听。';
      feedback.className = 'qc-feedback';
    }
    if (optsBox) {
      optsBox.innerHTML = '';
      q.options.forEach((text, i) => {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'qc-opt';
        b.textContent = `${String.fromCharCode(65 + i)}. ${text}`;
        b.onclick = () => pick(i);
        optsBox.appendChild(b);
      });
    }
    if (explainBtn) {
      explainBtn.disabled = true;
      explainBtn.classList.remove('ready');
      explainBtn.textContent = '听小侨讲';
    }
  }

  function pick(i) {
    if (answered) return;
    answered = true;
    const q = bank[idx];
    const ok = i === q.answer;
    if (optsBox) {
      Array.from(optsBox.children).forEach((b, k) => {
        b.disabled = true;
        if (k === q.answer) b.classList.add('right');
        else if (k === i) b.classList.add('wrong');
        else b.classList.add('dim');
      });
    }
    if (feedback) {
      const verdict = ok ? (q.right || '答对了！') : (q.wrong || '差一点。');
      feedback.textContent = `${verdict}　点「听小侨讲」，听小侨说说为什么。`;
      feedback.className = ok ? 'qc-feedback ok' : 'qc-feedback no';
    }
    if (explainBtn) {
      explainBtn.disabled = false;
      explainBtn.classList.add('ready');
    }
  }

  function explain() {
    if (!answered) return;
    const q = bank[idx];
    if (explainBtn) explainBtn.classList.remove('ready');
    let accepted = false;
    try {
      // force 只绕过「400ms 内重复提交」防抖，不改后端限流；失败时保留按钮可重试
      accepted = !!handlers.ask(q.followup, false, { force: true });
    } catch (e) {
      accepted = false;
    }
    if (!accepted && toast) toast('小侨还在讲上一句，稍等一下再点「听小侨讲」');
    if (accepted) {
      if (explainBtn) explainBtn.textContent = '再听一遍';
      setOpen(false);   // 收起题板，让右下角同步小窗里的数字人露出来开讲
    }
  }

  function setOpen(on) {
    const show = !!on;
    card.classList.toggle('show', show);
    entry.classList.toggle('on', show);
    // 答题卡片与展品特写时的「讲解员同步画面」都在右侧，会互相遮挡。
    // 答题阶段由 CSS 把同步画面缩回右下角让位（讲解员仍可见）；
    // 点「听小侨讲」后自动收起题板，画面恢复大尺寸，观众看着数字人听讲解。
    if (document.body) document.body.classList.toggle('quiz-open', show);
    if (show && !card.dataset.ready) {
      render();
      card.dataset.ready = '1';
    }
  }

  entry.onclick = () => setOpen(!card.classList.contains('show'));
  if (closeBtn) closeBtn.onclick = () => setOpen(false);
  if (nextBtn) nextBtn.onclick = () => { idx = (idx + 1) % bank.length; render(); };
  if (explainBtn) explainBtn.onclick = explain;
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && card.classList.contains('show')) setOpen(false);
  });

  return {
    available: true,
    count: bank.length,
    open: () => setOpen(true),
    close: () => setOpen(false),
  };
}
