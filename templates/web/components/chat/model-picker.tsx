"use client"

import type { AgentInfo } from "egin-ts/react"
import { RiArrowDownSLine } from "@remixicon/react"

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { InputGroupButton } from "@/components/ui/input-group"
import { effortName, modelName } from "@/lib/model-name"

const DEFAULT = "default"

interface ModelPickerProps {
  info: AgentInfo
  model: string
  onModel: (model: string) => void
  /** null: the agent's own setting, sent as nothing. */
  effort: string | null
  onEffort: (effort: string | null) => void
}

/** shadcn's Model Selector: the model, when the agent offers more than one, and its reasoning effort. */
export function ModelPicker({ info, model, onModel, effort, onEffort }: ModelPickerProps) {
  const efforts = info.reasoning_efforts[model] ?? []
  return (
    <>
      {info.models.length > 1 && (
        <DropdownMenu>
          <DropdownMenuTrigger render={<InputGroupButton size="sm" variant="outline" aria-label={`Model: ${modelName(model)}`} />}>
            {modelName(model)}
            <RiArrowDownSLine />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-56">
            <DropdownMenuGroup>
              <DropdownMenuLabel>Model</DropdownMenuLabel>
              <DropdownMenuRadioGroup value={model} onValueChange={(v) => onModel(String(v))}>
                {info.models.map((m) => (
                  <DropdownMenuRadioItem key={m} value={m} closeOnClick>
                    {modelName(m)}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      {efforts.length > 0 && (
        <DropdownMenu>
          <DropdownMenuTrigger render={<InputGroupButton size="sm" variant="outline" aria-label={`Effort: ${effort ? effortName(effort) : "Default"}`} />}>
            <span className="text-muted-foreground">Effort</span>
            {effort ? effortName(effort) : "Default"}
            <RiArrowDownSLine />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-40">
            <DropdownMenuGroup>
              <DropdownMenuLabel>Effort</DropdownMenuLabel>
              <DropdownMenuRadioGroup value={effort ?? DEFAULT} onValueChange={(v) => onEffort(v === DEFAULT ? null : String(v))}>
                <DropdownMenuRadioItem value={DEFAULT} closeOnClick>
                  Default
                </DropdownMenuRadioItem>
                {efforts.map((e) => (
                  <DropdownMenuRadioItem key={e} value={e} closeOnClick>
                    {effortName(e)}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </>
  )
}
