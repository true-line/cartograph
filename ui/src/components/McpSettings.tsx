import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { McpSettingsPanel, type McpStatus } from './McpSettingsPanel';

export function McpSettings({ available }: { available: boolean }) {
  const [status, setStatus] = useState<McpStatus>('unavailable');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [configuration, setConfiguration] = useState<string | null>(null);
  const generation = useRef(0);
  const run = useCallback(async (operation?: () => Promise<unknown>, showConfig = false) => {
    const ticket = ++generation.current;
    if (!available) { setStatus('unavailable'); setConfiguration(null); setBusy(false); return; }
    setBusy(true); setError(null); setConfiguration(null);
    try {
      if (operation) await operation();
      const current = await invoke<McpStatus>('mcp_reads_status');
      if (!['disabled', 'running', 'stopped', 'draining', 'cleanup_failed'].includes(current)) throw new Error('unavailable');
      if (ticket !== generation.current) return;
      setStatus(current);
      if (showConfig && current === 'running') {
        const config = await invoke<unknown>('mcp_reads_client_configuration');
        if (ticket === generation.current) setConfiguration(JSON.stringify(config, null, 2));
      }
    } catch (failure) {
      if (ticket === generation.current) setStatus('unavailable');
      if (ticket === generation.current) setError(String(failure) === 'unsupported_platform'
        ? 'Read-only MCP is not available on this platform.'
        : 'The request could not be completed. Check the resource policy and packaged relay, then refresh the service.');
    } finally { if (ticket === generation.current) setBusy(false); }
  }, [available]);
  useEffect(() => {
    const ticket = ++generation.current;
    if (available) {
      void invoke<McpStatus>('mcp_reads_status').then((current) => {
        if (ticket !== generation.current) return;
        if (!['disabled', 'running', 'stopped', 'draining', 'cleanup_failed'].includes(current)) {
          setStatus('unavailable');
          return;
        }
        setStatus(current);
        setConfiguration(null);
        setError(null);
        setBusy(false);
      }).catch(() => {
        if (ticket === generation.current) {
          setStatus('unavailable');
          setBusy(false);
        }
      });
    }
    return () => { generation.current += 1; };
  }, [available]);
  return <McpSettingsPanel status={available ? status : 'unavailable'} available={available} busy={available && busy} error={available ? error : null}
    configuration={available ? configuration : null} onRefresh={() => void run()}
    onEnable={(policy) => void run(() => invoke('mcp_reads_enable', { policy }))}
    onDisable={() => void run(() => invoke('mcp_reads_disable'))}
    onConfiguration={() => void run(undefined, true)} />;
}
