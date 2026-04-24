import { expectLogic } from 'kea-test-utils'

import { useMocks } from '~/mocks/jest'
import { initKeaTests } from '~/test/init'

import { dataDeletionLogic } from './dataDeletionLogic'

describe('dataDeletionLogic', () => {
    let logic: ReturnType<typeof dataDeletionLogic.build>

    beforeEach(() => {
        useMocks({
            get: {
                '/api/environments/:team_id/data_deletion_requests/': { results: [] },
            },
            post: {
                '/api/environments/:team_id/data_deletion_requests/preview/': {
                    count: 42,
                    min_timestamp: '2026-01-01T00:00:00Z',
                    max_timestamp: '2026-01-02T00:00:00Z',
                    rows: [
                        {
                            uuid: 'abc',
                            event: '$pageview',
                            timestamp: '2026-01-01T12:00:00Z',
                            distinct_id: 'user-1',
                            properties: '{}',
                        },
                    ],
                    limit: 3000,
                    truncated: false,
                },
            },
        })
        initKeaTests()
        logic = dataDeletionLogic()
        logic.mount()
    })

    it('starts on the new request tab with empty list', async () => {
        await expectLogic(logic).toFinishAllListeners()
        expect(logic.values.activeTab).toBe('new')
        expect(logic.values.deletionRequests).toEqual([])
    })

    it('marks form as scoped when event, start time, and predicate criteria line up', () => {
        expect(logic.values.previewScoped).toBe(false)
        logic.actions.setNewRequestValue('start_time', '2026-01-01T00:00:00Z')
        expect(logic.values.previewScoped).toBe(false)
        logic.actions.setNewRequestValue('events', ['$pageview'])
        expect(logic.values.previewScoped).toBe(true)
    })

    it('runs a preview after the form becomes scoped', async () => {
        logic.actions.setNewRequestValue('start_time', '2026-01-01T00:00:00Z')
        logic.actions.setNewRequestValue('events', ['$pageview'])
        await expectLogic(logic).toDispatchActions(['refreshPreview', 'refreshPreviewSuccess']).toFinishAllListeners()
        expect(logic.values.preview?.count).toBe(42)
        expect(logic.values.preview?.rows).toHaveLength(1)
    })

    it('clears the preview when the form loses scope', async () => {
        logic.actions.setNewRequestValue('start_time', '2026-01-01T00:00:00Z')
        logic.actions.setNewRequestValue('events', ['$pageview'])
        await expectLogic(logic).toDispatchActions(['refreshPreviewSuccess']).toFinishAllListeners()
        logic.actions.setNewRequestValue('start_time', null)
        await expectLogic(logic).toDispatchActions(['clearPreview']).toFinishAllListeners()
        expect(logic.values.preview).toBeNull()
    })

    it('flags property_removal as missing properties', () => {
        logic.actions.setNewRequestValue('request_type', 'property_removal')
        logic.actions.setNewRequestValue('start_time', '2026-01-01T00:00:00Z')
        expect(logic.values.previewScoped).toBe(false)
        logic.actions.setNewRequestValue('properties', ['$browser'])
        expect(logic.values.previewScoped).toBe(true)
    })
})
