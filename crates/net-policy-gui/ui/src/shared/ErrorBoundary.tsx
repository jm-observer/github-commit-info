import { Component, type ReactNode } from 'react'

interface Props { children: ReactNode }
interface State { error: Error | null }

export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  componentDidCatch(error: Error, info: { componentStack?: string }) {
    console.error('[ErrorBoundary]', error, info.componentStack)
  }

  render() {
    if (!this.state.error) return this.props.children
    return (
      <div className="m-8 max-w-2xl rounded-lg border border-red-300 bg-red-50 p-6 text-sm">
        <h2 className="mb-2 text-base font-semibold text-red-700">前端异常</h2>
        <pre className="whitespace-pre-wrap break-words text-red-800">
          {this.state.error.message}
          {'\n\n'}
          {this.state.error.stack}
        </pre>
      </div>
    )
  }
}
