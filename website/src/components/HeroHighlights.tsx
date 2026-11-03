import {useEffect, useRef, useState} from 'react';
import Link from '@docusaurus/Link';
import type {Locale} from '../../content.config';
import styles from './HeroHighlights.module.css';

const highlights = [
  {
    id: 'tokenless',
    name: 'Tokenless',
    lead: {en: 'Estimated tool-schema tokens reduced by ', zh: '参考样本中，工具 Schema 的估算 Token 减少 '},
    value: {en: '47.3%', zh: '47.3%'},
    tail: {en: ' in the reference test.', zh: '。'},
    note: {en: 'Tokenless · one Schema fixture · estimated tokens. ', zh: 'Tokenless 单个 Schema 样本，按估算 Token 计算。'},
    source: '/docs/user-guide/tokenless/measuring-savings/',
    sourceLabel: {en: 'See the measurement', zh: '查看测量方法'},
  },
  {
    id: 'ws-ckpt',
    name: 'ws-ckpt',
    lead: {en: 'Workspace rollback in ', zh: '历史压力测试中，工作区回滚耗时'},
    value: {en: '~108 ms', zh: '约 108 ms'},
    tail: {en: ' in a historical stress test.', zh: '。'},
    note: {en: 'ws-ckpt 0.4.2 · 10h soak · 40 GiB loop / ESSD PL1. Historical result; latency varies by workload and host. ', zh: 'ws-ckpt 0.4.2 · 10 小时持续压力测试 · 40 GiB loop / ESSD PL1。历史结果，耗时随工作负载与环境变化。'},
    source: 'https://github.com/agentic-os-org/ANOLISA/commit/c01dea7c69bfad1b3cbe39babca76aff7e6e83a1',
    sourceLabel: {en: 'Historical optimization commit', zh: '历史优化提交'},
  },
  {
    id: 'agentsight',
    name: 'AgentSight',
    lead: {en: 'Observe Agent runs with ', zh: '以'},
    value: {en: '0', zh: '零代码改动'},
    tail: {en: ' changes to Agent code.', zh: '接入 Agent 运行观测。'},
    note: {en: 'AgentSight · Linux/eBPF · root or CAP_BPF + CAP_PERFMON required. ', zh: 'AgentSight · Linux/eBPF · 需要 root 或 CAP_BPF + CAP_PERFMON。'},
    source: '/docs/user-guide/agentsight/',
    sourceLabel: {en: 'Explore observability', zh: '了解观测能力'},
  },
] as const;

const labels = {
  en: {region: 'ANOLISA capability highlights', slide: 'slide', choose: 'Show', pause: 'Pause', play: 'Play'},
  zh: {region: 'ANOLISA 能力亮点', slide: '幻灯片', choose: '查看', pause: '暂停', play: '播放'},
} as const;

export default function HeroHighlights({locale}: {locale: Locale}) {
  const t = labels[locale];
  const regionRef = useRef<HTMLElement>(null);
  const pointerRotationIntent = useRef<boolean | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const [playing, setPlaying] = useState(true);
  const [hovered, setHovered] = useState(false);
  const [inView, setInView] = useState(false);
  const [documentVisible, setDocumentVisible] = useState(true);
  const rotating = playing && !hovered && inView && documentVisible;

  useEffect(() => {
    const media = window.matchMedia('(prefers-reduced-motion: reduce)');
    const applyMotionPreference = () => { if (media.matches) setPlaying(false); };
    const updateVisibility = () => setDocumentVisible(document.visibilityState === 'visible');
    applyMotionPreference();
    updateVisibility();
    media.addEventListener('change', applyMotionPreference);
    document.addEventListener('visibilitychange', updateVisibility);
    const observer = new IntersectionObserver(([entry]) => setInView(entry.isIntersecting));
    if (regionRef.current) observer.observe(regionRef.current);
    return () => {
      observer.disconnect();
      media.removeEventListener('change', applyMotionPreference);
      document.removeEventListener('visibilitychange', updateVisibility);
    };
  }, []);

  useEffect(() => {
    if (!rotating) return undefined;
    const timer = window.setInterval(() => setActiveIndex((index) => (index + 1) % highlights.length), 7000);
    return () => window.clearInterval(timer);
  }, [rotating]);

  return (
    <section ref={regionRef} className={styles.highlights} aria-label={t.region}
      aria-roledescription={locale === 'zh' ? '轮播' : 'carousel'}
      onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)}
      onFocusCapture={() => setPlaying(false)}>
      <div className={styles.slides} aria-live={rotating ? 'off' : 'polite'} aria-atomic="true">
        {highlights.map((highlight, index) => (
          <article className={`${styles.slide} ${index === activeIndex ? styles.active : ''}`}
            role="group" aria-roledescription={t.slide} aria-label={`${index + 1} / ${highlights.length}: ${highlight.name}`}
            aria-hidden={index !== activeIndex} key={highlight.id}>
            <p className={styles.headline}>{highlight.lead[locale]}<strong>{highlight.value[locale]}</strong>{highlight.tail[locale]}</p>
            <p className={styles.note}>
              {highlight.note[locale]}
              <Link to={highlight.source} className={styles.sourceLink}
                tabIndex={index === activeIndex ? 0 : -1}>{highlight.sourceLabel[locale]}</Link>
            </p>
          </article>
        ))}
      </div>
      <div className={styles.controls}>
        <div className={styles.selectors} role="group" aria-label={t.choose}>
          {highlights.map((highlight, index) => (
            <button type="button" className={styles.selector} key={highlight.id}
              aria-label={`${t.choose} ${highlight.name}`} aria-current={index === activeIndex ? 'true' : undefined}
              onClick={() => { setActiveIndex(index); setPlaying(false); }} />
          ))}
        </div>
        <button type="button" className={styles.toggle} data-rotation-control="true"
          aria-label={`${playing ? t.pause : t.play} ${t.region}`}
          onPointerDown={() => {
            // Focus pauses rotation before click; preserve the pointer's original intent.
            pointerRotationIntent.current = !playing;
          }}
          onPointerCancel={() => { pointerRotationIntent.current = null; }}
          onClick={(event) => {
            const intent = event.detail > 0 ? pointerRotationIntent.current : null;
            pointerRotationIntent.current = null;
            setPlaying((value) => intent ?? !value);
          }}>
          <svg className={styles.toggleIcon} viewBox="0 0 16 16" aria-hidden="true" focusable="false">
            {playing
              ? <><rect x="4" y="3" width="3" height="10" rx="0.5" /><rect x="9" y="3" width="3" height="10" rx="0.5" /></>
              : <path d="M5 3.5v9l7-4.5z" />}
          </svg>
          <span>{playing ? t.pause : t.play}</span>
        </button>
      </div>
    </section>
  );
}
