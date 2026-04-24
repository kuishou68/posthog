import { actions, afterMount, connect, kea, listeners, path, reducers, selectors } from 'kea'
import { forms } from 'kea-forms'
import { loaders } from 'kea-loaders'

import { lemonToast } from '@posthog/lemon-ui'

import api, { PaginatedResponse } from 'lib/api'
import { dayjs } from 'lib/dayjs'
import { teamLogic } from 'scenes/teamLogic'

import type { dataDeletionLogicType } from './dataDeletionLogicType'

export type DataDeletionRequestType = 'event_removal' | 'property_removal'

export type DataDeletionStatus =
    | 'draft'
    | 'pending'
    | 'approved'
    | 'in_progress'
    | 'queued'
    | 'completed'
    | 'failed'

export interface DataDeletionRequest {
    id: string
    request_type: DataDeletionRequestType
    start_time: string
    end_time: string
    events: string[]
    delete_all_events: boolean
    hogql_predicate: string
    properties: string[]
    notes: string
    status: DataDeletionStatus
    approved: boolean
    approved_at: string | null
    execution_mode: 'immediate' | 'deferred'
    count: number | null
    min_timestamp: string | null
    max_timestamp: string | null
    stats_calculated_at: string | null
    created_by: { id: number; first_name: string; last_name: string; email: string } | null
    created_at: string
    updated_at: string
}

export interface DataDeletionPreview {
    count: number
    min_timestamp: string | null
    max_timestamp: string | null
    rows: {
        uuid: string
        event: string
        timestamp: string | null
        distinct_id: string
        properties: string
    }[]
    limit: number
    truncated: boolean
}

export type DataDeletionTab = 'new' | 'history'

export interface NewRequestFormValues {
    request_type: DataDeletionRequestType
    start_time: string | null
    end_time_through_now: boolean
    end_time: string | null
    events: string[]
    delete_all_events: boolean
    hogql_predicate: string
    properties: string[]
    notes: string
}

const DEFAULT_FORM: NewRequestFormValues = {
    request_type: 'event_removal',
    start_time: null,
    end_time_through_now: true,
    end_time: null,
    events: [],
    delete_all_events: false,
    hogql_predicate: '',
    properties: [],
    notes: '',
}

const PREVIEW_DEBOUNCE_MS = 500
const HISTORY_POLL_MS = 15_000

function resolveEndTime(values: NewRequestFormValues): string | null {
    if (values.end_time_through_now) {
        return dayjs().toISOString()
    }
    return values.end_time
}

function formIsSufficientlyScoped(values: NewRequestFormValues): boolean {
    if (!values.start_time) {
        return false
    }
    if (!values.end_time_through_now && !values.end_time) {
        return false
    }
    if (values.request_type === 'event_removal') {
        const hasEventScope = values.delete_all_events || values.events.length > 0
        const hasPredicate = values.hogql_predicate.trim().length > 0
        return hasEventScope || hasPredicate
    }
    return values.properties.length > 0
}

export const dataDeletionLogic = kea<dataDeletionLogicType>([
    path(['scenes', 'settings', 'project', 'DataDeletion', 'dataDeletionLogic']),
    connect(() => ({
        values: [teamLogic, ['currentTeamId']],
    })),
    actions({
        setActiveTab: (tab: DataDeletionTab) => ({ tab }),
        cancelRequest: (id: string) => ({ id }),
        startPolling: true,
        stopPolling: true,
    }),
    reducers({
        activeTab: [
            'new' as DataDeletionTab,
            {
                setActiveTab: (_, { tab }) => tab,
            },
        ],
        pollingActive: [
            false,
            {
                startPolling: () => true,
                stopPolling: () => false,
            },
        ],
    }),
    loaders(({ values }) => ({
        deletionRequests: [
            [] as DataDeletionRequest[],
            {
                loadDeletionRequests: async () => {
                    const response = await api.get<PaginatedResponse<DataDeletionRequest>>(
                        `api/environments/${values.currentTeamId}/data_deletion_requests/`
                    )
                    return response.results
                },
            },
        ],
        preview: [
            null as DataDeletionPreview | null,
            {
                clearPreview: () => null,
                refreshPreview: async (_, breakpoint) => {
                    await breakpoint(PREVIEW_DEBOUNCE_MS)
                    const form = values.newRequest
                    if (!formIsSufficientlyScoped(form)) {
                        return null
                    }
                    const payload = {
                        request_type: form.request_type,
                        start_time: form.start_time,
                        end_time: resolveEndTime(form),
                        events: form.events,
                        delete_all_events: form.delete_all_events,
                        hogql_predicate: form.hogql_predicate,
                        properties: form.properties,
                    }
                    try {
                        return await api.create<DataDeletionPreview>(
                            `api/environments/${values.currentTeamId}/data_deletion_requests/preview/`,
                            payload
                        )
                    } catch (error: any) {
                        // Preview failures are surfaced inline; swallow so the form stays usable.
                        const detail = error?.data ?? error?.detail ?? 'Preview failed'
                        lemonToast.error(
                            typeof detail === 'string' ? detail : 'Could not preview with current criteria.'
                        )
                        return null
                    }
                },
            },
        ],
    })),
    forms(({ actions, values }) => ({
        newRequest: {
            defaults: DEFAULT_FORM,
            errors: (form) => ({
                request_type: !form.request_type ? 'Pick a request type' : undefined,
                start_time: !form.start_time ? 'Start date is required' : undefined,
                end_time:
                    !form.end_time_through_now && !form.end_time
                        ? 'Pick an end date, or choose "through now"'
                        : undefined,
                properties:
                    form.request_type === 'property_removal' && form.properties.length === 0
                        ? 'Choose at least one property to remove'
                        : undefined,
                events:
                    form.request_type === 'event_removal' &&
                    !form.delete_all_events &&
                    form.events.length === 0 &&
                    !form.hogql_predicate.trim()
                        ? 'Pick events, enable "delete all events", or add a HogQL predicate'
                        : undefined,
            }),
            submit: async (form) => {
                const payload = {
                    request_type: form.request_type,
                    start_time: form.start_time,
                    end_time: resolveEndTime(form),
                    events: form.events,
                    delete_all_events: form.delete_all_events,
                    hogql_predicate: form.hogql_predicate,
                    properties: form.request_type === 'property_removal' ? form.properties : [],
                    notes: form.notes,
                }
                await api.create<DataDeletionRequest>(
                    `api/environments/${values.currentTeamId}/data_deletion_requests/`,
                    payload
                )
                lemonToast.success('Deletion request submitted for review')
                actions.resetNewRequest()
                actions.clearPreview()
                actions.loadDeletionRequests()
                actions.setActiveTab('history')
            },
        },
    })),
    selectors({
        previewScoped: [
            (s) => [s.newRequest],
            (form: NewRequestFormValues): boolean => formIsSufficientlyScoped(form),
        ],
        pendingCount: [
            (s) => [s.deletionRequests],
            (requests: DataDeletionRequest[]): number =>
                requests.filter((r) => ['pending', 'approved', 'in_progress', 'queued'].includes(r.status)).length,
        ],
    }),
    listeners(({ actions, values }) => ({
        setNewRequestValue: () => {
            if (formIsSufficientlyScoped(values.newRequest)) {
                actions.refreshPreview()
            } else if (values.preview) {
                actions.clearPreview()
            }
        },
        cancelRequest: async ({ id }) => {
            try {
                await api.delete(`api/environments/${values.currentTeamId}/data_deletion_requests/${id}/`)
                lemonToast.success('Request cancelled')
                actions.loadDeletionRequests()
            } catch {
                lemonToast.error('Could not cancel request')
            }
        },
        setActiveTab: ({ tab }) => {
            if (tab === 'history') {
                actions.loadDeletionRequests()
            } else {
                actions.stopPolling()
            }
        },
        startPolling: async (_, breakpoint) => {
            while (true) {
                await breakpoint(HISTORY_POLL_MS)
                if (!values.pollingActive) {
                    break
                }
                actions.loadDeletionRequests()
            }
        },
        loadDeletionRequestsSuccess: () => {
            if (values.activeTab === 'history' && !values.pollingActive) {
                actions.startPolling()
            }
        },
    })),
    afterMount(({ actions }) => {
        actions.loadDeletionRequests()
    }),
])
