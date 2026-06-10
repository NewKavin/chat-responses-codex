import type { Announcement } from '@/types'

export const buildAnnouncementSeenKey = (employeeId: string) =>
  `portal_announcement_seen:${employeeId}`

export const shouldShowAnnouncement = (
  announcement: Announcement | null | undefined,
  seenAnnouncementId?: string | null
) => Boolean(announcement && announcement.active && announcement.id !== seenAnnouncementId)
