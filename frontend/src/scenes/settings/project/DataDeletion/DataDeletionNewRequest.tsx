import { useActions, useValues } from 'kea'
import { Form } from 'kea-forms'
import { useState } from 'react'

import { LemonBanner, LemonButton, LemonInput, LemonModal, LemonSelect } from '@posthog/lemon-ui'

import { HogQLEditor } from 'lib/components/HogQLEditor/HogQLEditor'
import { TaxonomicFilterGroupType } from 'lib/components/TaxonomicFilter/types'
import { TaxonomicPopover } from 'lib/components/TaxonomicPopover/TaxonomicPopover'
import { dayjs } from 'lib/dayjs'
import { LemonCalendarSelectInput } from 'lib/lemon-ui/LemonCalendar/LemonCalendarSelect'
import { LemonCheckbox } from 'lib/lemon-ui/LemonCheckbox'
import { LemonField } from 'lib/lemon-ui/LemonField'

import { dataDeletionLogic } from './dataDeletionLogic'
import { DataDeletionPreviewPanel } from './DataDeletionPreviewPanel'

export function DataDeletionNewRequest(): JSX.Element {
    const { newRequest, isNewRequestSubmitting, newRequestHasErrors, preview, previewScoped } =
        useValues(dataDeletionLogic)
    const { setNewRequestValue, submitNewRequest } = useActions(dataDeletionLogic)
    const [confirmText, setConfirmText] = useState('')
    const [confirmOpen, setConfirmOpen] = useState(false)

    const isPropertyRemoval = newRequest.request_type === 'property_removal'

    return (
        <div className="flex flex-col gap-4">
            <LemonBanner type="warning">
                <div className="flex flex-col gap-1">
                    <b>Deletions are permanent and irreversible.</b>
                    <span>
                        Your request will be reviewed by PostHog before execution, typically within one business day.
                        Narrow the time range and use predicates to limit the scope.
                    </span>
                </div>
            </LemonBanner>

            <Form logic={dataDeletionLogic} formKey="newRequest" className="flex flex-col gap-3">
                <div className="flex flex-wrap items-center gap-2 text-base">
                    <span>I want to delete</span>
                    <LemonField name="request_type" inline>
                        <LemonSelect
                            options={[
                                { value: 'event_removal', label: 'events' },
                                { value: 'property_removal', label: 'properties from events' },
                            ]}
                            value={newRequest.request_type}
                            onChange={(value) => setNewRequestValue('request_type', value)}
                        />
                    </LemonField>

                    <span>from</span>
                    <LemonField name="start_time" inline>
                        <LemonCalendarSelectInput
                            value={newRequest.start_time ? dayjs(newRequest.start_time) : null}
                            onChange={(date) => setNewRequestValue('start_time', date ? date.toISOString() : null)}
                            placeholder="Start date"
                            granularity="minute"
                        />
                    </LemonField>

                    <span>to</span>
                    {newRequest.end_time_through_now ? (
                        <LemonButton type="secondary" onClick={() => setNewRequestValue('end_time_through_now', false)}>
                            now (click to pick)
                        </LemonButton>
                    ) : (
                        <LemonField name="end_time" inline>
                            <LemonCalendarSelectInput
                                value={newRequest.end_time ? dayjs(newRequest.end_time) : null}
                                onChange={(date) => setNewRequestValue('end_time', date ? date.toISOString() : null)}
                                placeholder="End date"
                                granularity="minute"
                            />
                        </LemonField>
                    )}
                </div>

                <LemonCheckbox
                    label='Use "now" as the end date (filled in when you submit)'
                    checked={newRequest.end_time_through_now}
                    onChange={(checked) => {
                        setNewRequestValue('end_time_through_now', checked)
                        if (checked) {
                            setNewRequestValue('end_time', null)
                        }
                    }}
                />

                {!isPropertyRemoval && (
                    <div className="flex flex-col gap-2">
                        <div className="flex flex-wrap items-center gap-2">
                            <span>where the event name is</span>
                            <TaxonomicPopover
                                groupType={TaxonomicFilterGroupType.Events}
                                value={null}
                                onChange={(value) => {
                                    if (!value || newRequest.events.includes(String(value))) {
                                        return
                                    }
                                    setNewRequestValue('events', [...newRequest.events, String(value)])
                                }}
                                placeholder={newRequest.events.length === 0 ? 'Pick events' : 'Add another event'}
                                type="secondary"
                                disabled={newRequest.delete_all_events}
                            />
                        </div>
                        {newRequest.events.length > 0 && (
                            <div className="flex flex-wrap gap-1">
                                {newRequest.events.map((name) => (
                                    <LemonButton
                                        key={name}
                                        size="xsmall"
                                        type="secondary"
                                        onClick={() =>
                                            setNewRequestValue(
                                                'events',
                                                newRequest.events.filter((e) => e !== name)
                                            )
                                        }
                                    >
                                        {name} ×
                                    </LemonButton>
                                ))}
                            </div>
                        )}
                        <LemonCheckbox
                            label="Delete every event in the time range (ignores the event filter)"
                            checked={newRequest.delete_all_events}
                            onChange={(checked) => {
                                setNewRequestValue('delete_all_events', checked)
                                if (checked) {
                                    setNewRequestValue('events', [])
                                }
                            }}
                        />
                    </div>
                )}

                {isPropertyRemoval && (
                    <div className="flex flex-col gap-2">
                        <div className="flex flex-wrap items-center gap-2">
                            <span>remove these properties from matching events:</span>
                            <TaxonomicPopover
                                groupType={TaxonomicFilterGroupType.EventProperties}
                                value={null}
                                onChange={(value) => {
                                    if (!value || newRequest.properties.includes(String(value))) {
                                        return
                                    }
                                    setNewRequestValue('properties', [...newRequest.properties, String(value)])
                                }}
                                placeholder="Pick properties"
                                type="secondary"
                            />
                        </div>
                        {newRequest.properties.length > 0 && (
                            <div className="flex flex-wrap gap-1">
                                {newRequest.properties.map((name) => (
                                    <LemonButton
                                        key={name}
                                        size="xsmall"
                                        type="secondary"
                                        onClick={() =>
                                            setNewRequestValue(
                                                'properties',
                                                newRequest.properties.filter((p) => p !== name)
                                            )
                                        }
                                    >
                                        {name} ×
                                    </LemonButton>
                                ))}
                            </div>
                        )}
                    </div>
                )}

                <LemonField
                    name="hogql_predicate"
                    label="Optional HogQL predicate (further narrows which events are matched)"
                >
                    <HogQLEditor
                        value={newRequest.hogql_predicate}
                        onChange={(value) => setNewRequestValue('hogql_predicate', value)}
                        placeholder="e.g. properties.$browser = 'Chrome'"
                        disableAutoFocus
                    />
                </LemonField>

                <LemonField name="notes" label="Notes for PostHog reviewers (optional)">
                    <LemonInput
                        value={newRequest.notes}
                        onChange={(value) => setNewRequestValue('notes', value)}
                        placeholder="Context that will help us review your request"
                    />
                </LemonField>
            </Form>

            <DataDeletionPreviewPanel />

            <div className="flex justify-end">
                <LemonButton
                    type="primary"
                    status="danger"
                    disabledReason={
                        !previewScoped
                            ? 'Fill in the scope to preview before submitting'
                            : !preview
                              ? 'Wait for the preview to load'
                              : preview.count === 0
                                ? 'Preview matches 0 events — nothing to delete'
                                : newRequestHasErrors
                                  ? 'Fix the form errors first'
                                  : undefined
                    }
                    onClick={() => setConfirmOpen(true)}
                    loading={isNewRequestSubmitting}
                >
                    Submit deletion request
                </LemonButton>
            </div>

            <LemonModal
                title="Confirm deletion request"
                isOpen={confirmOpen}
                onClose={() => setConfirmOpen(false)}
                footer={
                    <>
                        <LemonButton type="secondary" onClick={() => setConfirmOpen(false)}>
                            Cancel
                        </LemonButton>
                        <LemonButton
                            status="danger"
                            type="primary"
                            disabled={confirmText.toLowerCase() !== 'delete'}
                            loading={isNewRequestSubmitting}
                            onClick={() => {
                                setConfirmOpen(false)
                                setConfirmText('')
                                submitNewRequest()
                            }}
                        >
                            Submit for review
                        </LemonButton>
                    </>
                }
            >
                <p>
                    This request covers approximately <b>{preview?.count ?? 0}</b> events. Once approved by PostHog,
                    deletion is <b>permanent</b>. Type <b>delete</b> to confirm.
                </p>
                <LemonInput value={confirmText} onChange={setConfirmText} placeholder="delete" />
            </LemonModal>
        </div>
    )
}
