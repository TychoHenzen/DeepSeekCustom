import { Component, type ErrorInfo, type ReactNode } from 'react';

interface AppErrorBoundaryProps {
  children: ReactNode;
}

interface AppErrorBoundaryState {
  failed: boolean;
}

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  override state: AppErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): AppErrorBoundaryState {
    return { failed: true };
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error('DeepSeekCustom frontend render failed', error, info.componentStack);
  }

  override render(): ReactNode {
    if (this.state.failed) {
      return (
        <main className="fatal-fallback">
          <section aria-labelledby="fatal-render-title" role="alert">
            <p className="eyebrow">Fatal frontend error</p>
            <h1 id="fatal-render-title">The application cannot continue</h1>
            <p>A frontend render failed. Reload the page to request a fresh application state.</p>
          </section>
        </main>
      );
    }
    return this.props.children;
  }
}
