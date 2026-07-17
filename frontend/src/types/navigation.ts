import type { Component } from 'vue'

export interface AppNavItem {
  path: string
  label: string
  icon: Component
  group?: string
}
