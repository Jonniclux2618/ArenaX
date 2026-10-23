"use client";

import React, { useState, useMemo } from "react";
import { BracketData, BracketMatch, BracketPlayer, BracketRound } from "@/types/bracket";
import { Card } from "@/components/ui/Card";
import { Button } from "@/components/ui/Button";
import {
  Trophy,
  ChevronRight,
  ChevronDown,
  User,
  Clock,
  Sparkles,
  AlertTriangle,
  CheckCircle2,
  Table as TableIcon,
  LayoutGrid,
} from "lucide-react";
import Link from "next/link";
import { cn } from "@/lib/utils";

interface BracketViewProps {
  bracketData: BracketData;
  currentUserId?: string;
}

export function BracketView({ bracketData, currentUserId }: BracketViewProps) {
  const [viewMode, setViewMode] = useState<"tree" | "table">("tree");
  const [activeRound, setActiveRound] = useState<number>(1);

  const sections = bracketData.sections || [];
  const primarySection = sections[0];
  const rounds = primarySection?.rounds || [];

  return (
    <div className="space-y-6" role="region" aria-label="Tournament Bracket View">
      {/* View Switcher */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Button
            variant={viewMode === "tree" ? "primary" : "outline"}
            size="sm"
            onClick={() => setViewMode("tree")}
            className="gap-2"
            aria-pressed={viewMode === "tree"}
          >
            <LayoutGrid className="h-4 w-4" aria-hidden="true" />
            Tree View
          </Button>
          <Button
            variant={viewMode === "table" ? "primary" : "outline"}
            size="sm"
            onClick={() => setViewMode("table")}
            className="gap-2"
            aria-pressed={viewMode === "table"}
          >
            <TableIcon className="h-4 w-4" aria-hidden="true" />
            Table View
          </Button>
        </div>

        {/* Legend */}
        <div className="hidden sm:flex items-center gap-4 text-xs text-muted-foreground" aria-label="Status legend">
          <span className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full bg-emerald-500" />
            Live
          </span>
          <span className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full bg-blue-500" />
            Completed
          </span>
          <span className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full bg-amber-500" />
            Bye
          </span>
          <span className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full bg-rose-500" />
            Dispute
          </span>
        </div>
      </div>

      {viewMode === "table" ? (
        <div className="overflow-x-auto rounded-lg border border-border bg-card">
          <table className="w-full text-left text-sm" role="table" aria-label="Tournament Match Schedule">
            <thead className="border-b border-border bg-muted/50 text-xs uppercase text-muted-foreground">
              <tr>
                <th scope="col" className="p-3">Round</th>
                <th scope="col" className="p-3">Player 1</th>
                <th scope="col" className="p-3">Player 2</th>
                <th scope="col" className="p-3">Status</th>
                <th scope="col" className="p-3">Score</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {rounds.flatMap((round) =>
                round.matches.map((match) => {
                  const isBye = Boolean(match.player1 && !match.player2);
                  return (
                    <tr key={match.id} className="hover:bg-muted/30">
                      <td className="p-3 font-medium">{round.name}</td>
                      <td className="p-3">
                        <span className={cn(match.winnerId && match.player1?.id === match.winnerId && "font-bold text-success")}>
                          {match.player1?.username ?? "TBD"}
                        </span>
                      </td>
                      <td className="p-3">
                        {isBye ? (
                          <span className="text-amber-500 font-medium">[Bye]</span>
                        ) : (
                          <span className={cn(match.winnerId && match.player2?.id === match.winnerId && "font-bold text-success")}>
                            {match.player2?.username ?? "TBD"}
                          </span>
                        )}
                      </td>
                      <td className="p-3">
                        <MatchStatusBadge status={match.status} isBye={isBye} />
                      </td>
                      <td className="p-3 font-mono">
                        {typeof match.scorePlayer1 === "number" && typeof match.scorePlayer2 === "number"
                          ? `${match.scorePlayer1} - ${match.scorePlayer2}`
                          : "-"}
                      </td>
                    </tr>
                  );
                }),
              )}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="overflow-x-auto rounded-lg border border-border bg-card/40 p-4">
          <div className="flex gap-8 min-w-max">
            {rounds.map((round) => (
              <div key={round.id} className="w-64 space-y-3">
                <div className="border-b border-border pb-2">
                  <h4 className="text-sm font-semibold">{round.name}</h4>
                </div>
                <div className="space-y-4">
                  {round.matches.map((match) => {
                    const isBye = Boolean(match.player1 && !match.player2);
                    return (
                      <div
                        key={match.id}
                        tabIndex={0}
                        role="region"
                        aria-label={`Match: ${match.player1?.username ?? "TBD"} vs ${match.player2?.username ?? (isBye ? "Bye" : "TBD")}. Status: ${match.status}`}
                        style={{ contentVisibility: "auto", containIntrinsicSize: "0 80px" }}
                        className="rounded-lg border border-border bg-card p-3 shadow-sm hover:border-primary/50 transition-colors"
                      >
                        <div className="flex items-center justify-between pb-2 border-b border-border/50 text-xs">
                          <span className="text-muted-foreground font-mono">#{match.id.slice(-4)}</span>
                          <MatchStatusBadge status={match.status} isBye={isBye} />
                        </div>
                        <div className="mt-2 space-y-1 text-sm">
                          <div className="flex justify-between items-center">
                            <span className={cn(match.winnerId && match.player1?.id === match.winnerId && "font-bold text-success")}>
                              {match.player1?.username ?? "TBD"}
                            </span>
                            <span className="font-mono text-xs">{match.scorePlayer1 ?? "-"}</span>
                          </div>
                          <div className="flex justify-between items-center">
                            {isBye ? (
                              <span className="text-amber-500 text-xs font-semibold">[Bye]</span>
                            ) : (
                              <span className={cn(match.winnerId && match.player2?.id === match.winnerId && "font-bold text-success")}>
                                {match.player2?.username ?? "TBD"}
                              </span>
                            )}
                            <span className="font-mono text-xs">{match.scorePlayer2 ?? "-"}</span>
                          </div>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function MatchStatusBadge({ status, isBye = false }: { status: BracketMatch["status"]; isBye?: boolean }) {
  if (isBye) {
    return <span className="rounded bg-amber-500/10 px-2 py-0.5 text-[10px] font-semibold text-amber-500 uppercase">Bye</span>;
  }
  const config = {
    pending: { label: "Pending", cls: "bg-muted text-muted-foreground" },
    ready: { label: "Ready", cls: "bg-blue-500/10 text-blue-500" },
    in_progress: { label: "Live", cls: "bg-emerald-500/10 text-emerald-500 animate-pulse font-semibold" },
    completed: { label: "Finished", cls: "bg-purple-500/10 text-purple-500 font-medium" },
    disputed: { label: "Disputed", cls: "bg-destructive/10 text-destructive font-bold" },
  }[status] || { label: status, cls: "bg-muted text-muted-foreground" };

  return <span className={cn("rounded px-2 py-0.5 text-[10px] uppercase", config.cls)}>{config.label}</span>;
}
