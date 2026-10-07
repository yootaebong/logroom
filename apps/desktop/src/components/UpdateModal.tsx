import { useMutation, useQuery } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { Download, Sparkles } from "lucide-react";
import { Dialog } from "radix-ui";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { useT } from "@/i18n";
import { getAppVersion, installUpdate, queryKeys, restartApp } from "@/lib/api";
import type { UpdateProgress } from "@/lib/types";
import { useAppStore } from "@/store";

function formatMb(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * 최초 실행 시 구버전이면 뜨는 업데이트 안내 모달(사용자 요구) — 백그라운드 체크(update.rs,
 * 시작 3초 후)가 새 버전을 찾으면 store `updateAvailable`이 채워지고, 이 모달이 자동으로 열린다.
 * 배지(사이드바 설정 아이콘)보다 강한 발견성을 주되, **설치는 여전히 사용자 클릭으로만** 진행한다
 * (ADR-0013 "설치는 명시적" 유지 — 몰래 자동 설치가 아니라 강조된 안내 + 원클릭 설치).
 *
 * "나중에"는 이 앱 세션 동안만 닫는다(dismissed) — 다음 실행 때 여전히 구버전이면 다시 뜬다.
 */
export function UpdateModal() {
  const t = useT();
  const updateAvailable = useAppStore((state) => state.updateAvailable);
  const [dismissed, setDismissed] = useState(false);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [installed, setInstalled] = useState(false);

  const { data: currentVersion } = useQuery({
    queryKey: queryKeys.appVersion(),
    queryFn: getAppVersion,
  });

  const installMutation = useMutation({
    mutationFn: installUpdate,
    onMutate: () => {
      setProgress(null);
      setInstalled(false);
    },
    onSuccess: () => setInstalled(true),
  });

  const open = updateAvailable !== null && !dismissed;

  // 설치 진행률 구독 — 모달이 열려 있는 동안에만(설정의 UpdateSection과 동일 패턴).
  useEffect(() => {
    if (!open) return;
    const unlistenPromise = listen<UpdateProgress>("update-progress", (event) => {
      setProgress(event.payload);
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [open]);

  if (updateAvailable === null) return null;

  const progressLabel =
    progress === null
      ? null
      : progress.total === null
        ? formatMb(progress.downloaded)
        : `${formatMb(progress.downloaded)} / ${formatMb(progress.total)}`;

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        // 설치 중에는 닫기를 막는다(진행 중 중단 방지). 그 외에는 "나중에"로 닫을 수 있다.
        if (!next && !installMutation.isPending) setDismissed(true);
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-background/80 backdrop-blur-sm" />
        <Dialog.Content className="fixed top-1/2 left-1/2 z-50 w-full max-w-sm -translate-x-1/2 -translate-y-1/2 rounded-lg border border-border bg-popover p-5 text-popover-foreground shadow-lg">
          <div className="flex items-start gap-3">
            <Sparkles className="mt-0.5 size-5 shrink-0 text-primary" aria-hidden="true" />
            <div className="min-w-0">
              <Dialog.Title className="text-base font-semibold">
                {t("update.modal.title")}
              </Dialog.Title>
              <Dialog.Description className="mt-1 text-sm text-muted-foreground">
                {t("update.modal.description", {
                  current: currentVersion ?? "—",
                  next: updateAvailable.version,
                })}
              </Dialog.Description>
            </div>
          </div>

          {installMutation.isPending && (
            <div className="mt-4">
              <p className="text-xs text-muted-foreground">
                {progressLabel
                  ? t("update.modal.downloading", { progress: progressLabel })
                  : t("update.modal.preparing")}
              </p>
            </div>
          )}

          {installMutation.isError && (
            <p className="mt-3 text-xs text-destructive">{t("update.modal.error")}</p>
          )}

          <div className="mt-5 flex items-center justify-end gap-2">
            {installed ? (
              <Button type="button" variant="default" size="sm" onClick={() => void restartApp()}>
                {t("update.modal.restartButton")}
              </Button>
            ) : (
              <>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  disabled={installMutation.isPending}
                  onClick={() => setDismissed(true)}
                >
                  {t("update.modal.laterButton")}
                </Button>
                <Button
                  type="button"
                  variant="default"
                  size="sm"
                  disabled={installMutation.isPending}
                  onClick={() => installMutation.mutate()}
                >
                  <Download className="size-4" aria-hidden="true" />
                  {installMutation.isPending
                    ? t("update.modal.installing")
                    : t("update.modal.installButton")}
                </Button>
              </>
            )}
          </div>

          {installed && (
            <p className="mt-3 text-xs text-muted-foreground">{t("update.modal.installedNote")}</p>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
