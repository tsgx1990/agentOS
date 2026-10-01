import type { ProviderInfo } from "../lib/providers";
import "./OnboardingProviderStep.css";

export interface OnboardingProviderStepProps {
  providers: ProviderInfo[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

const GROUPS: { key: "cn" | "intl"; label: string }[] = [
  { key: "cn", label: "国内" },
  { key: "intl", label: "国际" },
];

/** 向导里带「推荐」标的服务：国际首选 Anthropic，国内首选 DeepSeek。 */
const RECOMMENDED = new Set(["anthropic", "deepseek"]);

/** 向导「选择服务」：国内 / 国际两组 3 列卡片，只选不填密钥（密钥在下一步）。自定义服务不进向导。 */
export function OnboardingProviderStep(p: OnboardingProviderStepProps) {
  const native = p.providers.filter((x) => x.native);
  return (
    <div className="ob-providers">
      {GROUPS.map((g) => (
        <div key={g.key} className="ob-group">
          <div className="ob-group-title">{g.label}</div>
          <div className="ob-grid">
            {native
              .filter((x) => x.region === g.key)
              .map((x) => {
                const active = x.id === p.selectedId;
                return (
                  <button
                    key={x.id}
                    type="button"
                    aria-pressed={active}
                    className={`ob-choice${active ? " is-active" : ""}`}
                    onClick={() => p.onSelect(x.id)}
                  >
                    <span className="ob-choice-name">{x.display}</span>
                    {RECOMMENDED.has(x.id) && <span className="ob-rec">推荐</span>}
                  </button>
                );
              })}
          </div>
        </div>
      ))}
      <p className="ob-more">其他服务稍后可在「模型与密钥」里添加。</p>
    </div>
  );
}
