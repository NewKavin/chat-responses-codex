import { describe, expect, it } from 'vitest'
import { buildAnnouncementSeenKey, shouldShowAnnouncement } from './announcement'

describe('announcement helpers', () => {
  it('builds the seen key from employee id', () => {
    expect(buildAnnouncementSeenKey('team-a')).toBe('portal_announcement_seen:team-a')
  })

  it('keeps keys isolated per employee', () => {
    expect(buildAnnouncementSeenKey('alice')).not.toBe(buildAnnouncementSeenKey('bob'))
  })

  it('shows a new active announcement when the id differs', () => {
    expect(
      shouldShowAnnouncement(
        {
          id: 'ann-1',
          title: '系统公告',
          content: '正文',
          level: 'warning',
          active: true,
          updated_at: 1
        },
        'ann-0'
      )
    ).toBe(true)
  })

  it('hides an announcement that was already seen', () => {
    expect(
      shouldShowAnnouncement(
        {
          id: 'ann-1',
          title: '系统公告',
          content: '正文',
          level: 'warning',
          active: true,
          updated_at: 1
        },
        'ann-1'
      )
    ).toBe(false)
  })

  it('hides an inactive announcement', () => {
    expect(
      shouldShowAnnouncement(
        {
          id: 'ann-2',
          title: '系统公告',
          content: '正文',
          level: 'warning',
          active: false,
          updated_at: 1
        },
        null
      )
    ).toBe(false)
  })
})
