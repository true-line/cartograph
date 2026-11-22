import { useState } from 'react';

/** One project as the host lists it (#1994). */
export interface ProjectEntry {
  id: string;
  name: string | null;
  /** Display names of its registered repositories when it was set aside. */
  repos: string[];
  active: boolean;
}

export interface ProjectsCardProps {
  projects: ProjectEntry[] | null;
  /** Repos of the loaded project, as the Workspace states them. */
  loadedRepos: string[];
  /** False while work runs: switching restarts Cartograph. */
  canSwitch: boolean;
  busy: boolean;
  error: string | null;
  onSwitch: (id: string) => void;
  onRename: (id: string, name: string) => void;
  onDelete: (id: string) => void;
}

function label(project: ProjectEntry, loadedRepos: string[]): string {
  if (project.name) return project.name;
  const repos = project.active ? loadedRepos : project.repos;
  return repos.length > 0 ? repos.join(', ') : 'Empty project';
}

/** Every project, the loaded one first: switch, rename or delete (#1994,
 * AC-1014). Switching restarts Cartograph into the chosen project with no
 * re-ingest; settings are shared by every project. */
export function ProjectsCard({
  projects,
  loadedRepos,
  canSwitch,
  busy,
  error,
  onSwitch,
  onRename,
  onDelete,
}: ProjectsCardProps) {
  const [confirming, setConfirming] = useState<{ id: string; action: 'switch' | 'delete' } | null>(
    null,
  );
  const [renaming, setRenaming] = useState<{ id: string; name: string } | null>(null);
  if (projects === null) return null;

  return (
    <section className="card projects-card">
      <h2>Projects</h2>
      <ul className="project-list">
        {projects.map((project) => {
          const name = label(project, loadedRepos);
          const asking = confirming?.id === project.id ? confirming.action : null;
          return (
            <li key={project.id} className="project-row">
              {renaming?.id === project.id ? (
                <form
                  onSubmit={(event) => {
                    event.preventDefault();
                    onRename(project.id, renaming.name);
                    setRenaming(null);
                  }}
                >
                  <input
                    aria-label={`Name for ${name}`}
                    value={renaming.name}
                    maxLength={120}
                    onChange={(event) => setRenaming({ id: project.id, name: event.target.value })}
                  />
                  <button type="submit" className="secondary-button">
                    Save
                  </button>
                  <button
                    type="button"
                    className="secondary-button"
                    onClick={() => setRenaming(null)}
                  >
                    Cancel
                  </button>
                </form>
              ) : (
                <span className="project-name" title={name}>
                  {name}
                  {project.active && <span className="badge"> Loaded</span>}
                </span>
              )}
              {asking === null && renaming?.id !== project.id && (
                <span className="project-actions">
                  <button
                    type="button"
                    className="secondary-button"
                    disabled={busy}
                    onClick={() => setRenaming({ id: project.id, name: project.name ?? '' })}
                  >
                    Rename
                  </button>
                  {!project.active && (
                    <>
                      <button
                        type="button"
                        className="secondary-button"
                        disabled={!canSwitch || busy}
                        onClick={() => setConfirming({ id: project.id, action: 'switch' })}
                      >
                        Switch
                      </button>
                      <button
                        type="button"
                        className="secondary-button"
                        disabled={busy}
                        onClick={() => setConfirming({ id: project.id, action: 'delete' })}
                      >
                        Delete
                      </button>
                    </>
                  )}
                </span>
              )}
              {asking !== null && (
                <div className="clear-confirmation" role="alert">
                  <p>
                    {asking === 'switch'
                      ? `Switch to ${name}? The loaded project is set aside as it is, and Cartograph restarts with ${name} loaded. Settings are shared by every project.`
                      : `Delete ${name}? Its recovered facts, findings, investigations, drafts, reviews, decisions and job history are removed for good.`}
                  </p>
                  <div className="clear-confirmation-actions">
                    <button
                      type="button"
                      className="secondary-button"
                      onClick={() => setConfirming(null)}
                    >
                      Keep
                    </button>
                    <button
                      type="button"
                      className={asking === 'switch' ? undefined : 'danger-button'}
                      onClick={() => {
                        setConfirming(null);
                        if (asking === 'switch') onSwitch(project.id);
                        else onDelete(project.id);
                      }}
                    >
                      {asking === 'switch' ? 'Switch and restart' : 'Delete project'}
                    </button>
                  </div>
                </div>
              )}
            </li>
          );
        })}
      </ul>
      {error && <p className="error-text">{error}</p>}
    </section>
  );
}
