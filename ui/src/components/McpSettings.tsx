import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { McpTaskSettings } from './McpTaskSettings';
import { McpSettingsPanel, type McpClientSkill, type McpStatus } from './McpSettingsPanel';

export function McpSettings({ available }: { available: boolean }) {
  return <McpSettingsSession key={String(available)} available={available} />;
}

function McpSettingsSession({ available }: { available: boolean }) {
  const [status, setStatus] = useState<McpStatus>('unavailable');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [configuration, setConfiguration] = useState<string | null>(null);
  const [skill, setSkill] = useState<McpClientSkill | null>(null);
  const [skillBusy, setSkillBusy] = useState(false);
  const skillGeneration = useRef(0);
  const generation = useRef(0);
  useEffect(() => () => { skillGeneration.current += 1; }, []);
  const showSkill = async () => {
    const ticket = ++skillGeneration.current;
    if (!available) return;
    setSkillBusy(true);
    setSkill(null);
    try {
      const result = await invoke<McpClientSkill>('mcp_reads_client_skill');
      if (ticket === skillGeneration.current) setSkill(result);
    } catch {
      if (ticket === skillGeneration.current) setSkill({ status: 'unavailable' });
    } finally {
      if (ticket === skillGeneration.current) setSkillBusy(false);
    }
  };
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
        ? 'MCP access is not available on this platform.'
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
    skill={available ? skill : null} skillBusy={available && skillBusy} onSkill={() => void showSkill()}
    renderTaskControls={(policy) => <McpTaskSettings available={available} serviceBusy={busy} status={status} policy={policy}
      onGrant={(chosenPolicy, clientId, maximumIdentities) => void run(() => invoke('mcp_tasks_enable', { policy: chosenPolicy, clientId, maximumIdentities }))} />}
    configuration={available ? configuration : null} onRefresh={() => void run()}
    onEnable={(policy) => void run(() => invoke('mcp_reads_enable', { policy }))}
    onDisable={() => void run(() => invoke('mcp_reads_disable'))}
    onRotate={() => void run(() => invoke('mcp_reads_rotate'))}
    onConfiguration={() => void run(undefined, true)} />;
}
