import React from 'react';
import clsx from 'clsx';
import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';
import useBaseUrl from '@docusaurus/useBaseUrl';
import Translate, {translate} from '@docusaurus/Translate';
import {
  Monitor,
  ShieldCheck,
  Gauge,
  Languages,
  ScanSearch,
  Layers,
  ArrowRight,
  Download,
  BookOpen,
} from 'lucide-react';
import styles from './index.module.css';

export default function Home(): React.JSX.Element {
  const { siteConfig } = useDocusaurusContext();
  const [activeShot, setActiveShot] = React.useState(0);

  const features = [
    {
      icon: Monitor,
      title: translate({id: 'home.feature.platform.title', message: '四个平台组合全覆盖'}),
      description: translate({
        id: 'home.feature.platform.description',
        message: 'Windows 与 Linux、x86_64 与 arm64 同时支持，这样的刻录软件很少，银河麒麟常跑的 arm64 整机也覆盖。',
      }),
    },
    {
      icon: ShieldCheck,
      title: translate({id: 'home.feature.guard.title', message: '写前检查'}),
      description: translate({
        id: 'home.feature.guard.description',
        message: '盘片没就绪会等待、已封口拒绝写入、盘被挂载提示卸载，把不稳定状态挡在写入之前。',
      }),
    },
    {
      icon: Gauge,
      title: translate({id: 'home.feature.progress.title', message: '实时进度与中止'}),
      description: translate({
        id: 'home.feature.progress.description',
        message: '刻录与追加的进度逐百分比可见，任务可以中途停止，不用守着终端猜状态。',
      }),
    },
    {
      icon: Languages,
      title: translate({id: 'home.feature.errors.title', message: '失败原因说中文'}),
      description: translate({
        id: 'home.feature.errors.description',
        message: '光驱断连、设备被占用等成因归类成中文说明，带上对盘的影响与下一步，不再猜英文报错。',
      }),
    },
    {
      icon: ScanSearch,
      title: translate({id: 'home.feature.verify.title', message: '回读校验'}),
      description: translate({
        id: 'home.feature.verify.description',
        message: '把盘上内容读回来与源逐文件对比，差异列成中文清单。',
      }),
    },
    {
      icon: Layers,
      title: translate({id: 'home.feature.append.title', message: '多区段追加'}),
      description: translate({
        id: 'home.feature.append.description',
        message: '默认不封盘，append 把新文件写进新区段，旧区段的文件保持可见。',
      }),
    },
  ];

  const screenshots = [
    {src: useBaseUrl('/img/screenshot-burn.png'), alt: translate({id: 'home.screenshot.burn', message: '刻录页'})},
    {src: useBaseUrl('/img/screenshot-burn-progress.png'), alt: translate({id: 'home.screenshot.progress', message: '刻录进行中'})},
    {src: useBaseUrl('/img/screenshot-devices.png'), alt: translate({id: 'home.screenshot.devices', message: '设备页查看盘上文件'})},
    {src: useBaseUrl('/img/screenshot-append.png'), alt: translate({id: 'home.screenshot.append', message: '追加页'})},
  ];

  React.useEffect(() => {
    const timer = setInterval(() => {
      setActiveShot((prev) => (prev + 1) % screenshots.length);
    }, 4000);
    return () => clearInterval(timer);
  }, []);

  return (
    <Layout
      title={siteConfig.title}
      description={translate({id: 'home.meta.description', message: '把文件刻进光盘'})}
    >
      {/* 首屏 */}
      <header className={styles.heroBanner}>
        <div className={styles.heroGlow} />
        <div className={styles.heroGrid} />
        <div className={clsx('container', styles.heroInner)}>
          <div className={styles.heroBadge}>
            <span className={styles.heroBadgeDot} />
            <Translate id="home.hero.badge">Linux 与 Windows，x86_64 与 arm64</Translate>
          </div>
          <h1 className={styles.heroTitle}>
            <Translate id="home.hero.title.line1">把文件，</Translate>
            <br />
            <span className={styles.heroTitleAccent}>
              <Translate id="home.hero.title.line2">刻进光盘</Translate>
            </span>
          </h1>
          <p className={styles.heroTagline}>
            <Translate
              id="home.hero.tagline.line1"
              description="hero tagline, first line">
              Windows 与 Linux、x86_64 与 arm64 全覆盖的刻录工具，很少。
            </Translate>
            <br />
            <Translate
              id="home.hero.tagline.line2"
              description="hero tagline, second line">
              Ubuntu、银河麒麟上的刻录难与不稳定，用写前检查、实时进度与回读校验一次做稳。
            </Translate>
          </p>
          <div className={styles.buttons}>
            <Link
              className={clsx('button', styles.btnPrimary)}
              to="/docs/quick-start"
            >
              <Download size={18} />
              <Translate id="home.cta.quickStart">快速开始</Translate>
            </Link>
            <Link
              className={clsx('button', styles.btnSecondary)}
              to="/docs/usage"
            >
              <BookOpen size={18} />
              <Translate id="home.cta.usage">命令行用法</Translate>
            </Link>
            <Link
              className={clsx('button', styles.btnGhost)}
              href="https://github.com/ouyangjiahong26/optiburn"
            >
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M15 22v-4a4.8 4.8 0 0 0-1-3.5c3 0 6-2 6-5.5.08-1.25-.27-2.48-1-3.5.28-1.15.28-2.35 0-3.5 0 0-1 0-3 1.5-2.64-.5-5.36-.5-8 0C6 2 5 2 5 2c-.3 1.15-.3 2.35 0 3.5A5.403 5.403 0 0 0 4 9c0 3.5 3 5.5 6 5.5-.39.49-.68 1.05-.85 1.65-.17.6-.22 1.23-.15 1.85v4"/>
                <path d="M9 18c-4.51 2-5-2-7-2"/>
              </svg>
              GitHub
            </Link>
          </div>

          {/* 截图展示 */}
          <div className={styles.showcase}>
            <div className={styles.showcaseFrame}>
              {screenshots.map((s, i) => (
                <img
                  key={s.src}
                  src={s.src}
                  alt={s.alt}
                  className={clsx(
                    styles.showcaseImg,
                    i === activeShot && styles.showcaseImgActive
                  )}
                />
              ))}
              <div className={styles.showcaseOverlay} />
            </div>
            <div className={styles.showcaseDots}>
              {screenshots.map((_, i) => (
                <button
                  key={i}
                  className={clsx(
                    styles.showcaseDot,
                    i === activeShot && styles.showcaseDotActive
                  )}
                  onClick={() => setActiveShot(i)}
                  aria-label={translate(
                    {id: 'home.screenshot.viewN', message: '查看截图 {n}'},
                    {n: i + 1},
                  )}
                />
              ))}
            </div>
          </div>
        </div>
      </header>

      <main>
        {/* 三步上手 */}
        <section className={styles.howItWorks}>
          <div className="container">
            <p className={styles.sectionKicker}>
              <Translate id="home.steps.kicker">三步上手</Translate>
            </p>
            <h2 className={styles.sectionTitle}>
              <Translate id="home.steps.title">从目录到光盘，一条流水线</Translate>
            </h2>
            <div className={styles.steps}>
              <div className={styles.step}>
                <div className={styles.stepNum}>1</div>
                <h3><Translate id="home.steps.install.title">装好工具</Translate></h3>
                <p>
                  <Translate id="home.steps.install.description">
                    cargo build 或下载安装包。刻录需要 PATH 上有 xorriso，写权限来自 cdrom 组。
                  </Translate>
                </p>
              </div>
              <ArrowRight className={styles.stepArrow} size={24} />
              <div className={styles.step}>
                <div className={styles.stepNum}>2</div>
                <h3><Translate id="home.steps.build.title">做镜像</Translate></h3>
                <p>
                  <Translate id="home.steps.build.description">
                    一条命令或图形界面。镜像覆盖 ISO 9660、Joliet 与 UDF Bridge，老设备与 Windows 都能读。
                  </Translate>
                </p>
              </div>
              <ArrowRight className={styles.stepArrow} size={24} />
              <div className={styles.step}>
                <div className={styles.stepNum}>3</div>
                <h3><Translate id="home.steps.burn.title">刻录与校验</Translate></h3>
                <p>
                  <Translate id="home.steps.burn.description">
                    写前检查盘片状态，进度实时可见。写完回读校验，追加时旧文件保持可见。
                  </Translate>
                </p>
              </div>
            </div>
          </div>
        </section>

        {/* 功能 */}
        <section className={styles.features}>
          <div className="container">
            <p className={styles.sectionKicker}>
              <Translate id="home.features.kicker">功能特点</Translate>
            </p>
            <h2 className={styles.sectionTitle}>
              <Translate id="home.features.title">刻盘这件事，值得一个现代工具</Translate>
            </h2>
            <div className={styles.featureGrid}>
              {features.map((f) => {
                const Icon = f.icon;
                return (
                  <div key={f.title} className={styles.featureCard}>
                    <div className={styles.featureIcon}>
                      <Icon size={22} strokeWidth={2} />
                    </div>
                    <h3>{f.title}</h3>
                    <p>{f.description}</p>
                  </div>
                );
              })}
            </div>
          </div>
        </section>

        {/* 行动号召 */}
        <section className={styles.cta}>
          <div className="container">
            <div className={styles.ctaBox}>
              <h2 className={styles.ctaTitle}>
                <Translate id="home.cta.finalTitle">准备好刻下一张盘了吗？</Translate>
              </h2>
              <p className={styles.ctaSubtitle}>
                <Translate id="home.cta.finalSubtitle">
                  Rust 一条命令，或图形界面点两下
                </Translate>
              </p>
              <div className={styles.buttons}>
                <Link
                  className={clsx('button', styles.btnPrimary)}
                  to="/docs/quick-start"
                >
                  <Translate id="home.cta.quickStart">快速开始</Translate>{' '}
                  <ArrowRight size={18} />
                </Link>
                <Link
                  className={clsx('button', styles.btnGhost)}
                  href="https://github.com/ouyangjiahong26/optiburn/releases"
                >
                  <Translate id="home.cta.download">下载最新版</Translate>
                </Link>
              </div>
            </div>
          </div>
        </section>
      </main>
    </Layout>
  );
}
