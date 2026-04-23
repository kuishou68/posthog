// AUTO-GENERATED from products/tasks/mcp/tools.yaml + OpenAPI — do not edit
import { z } from 'zod'

import type { Schemas } from '@/api/generated'
import {
    SandboxListQueryParams,
    SandboxRetrieveParams,
    TaskAutomationsCreateBody,
    TaskAutomationsDestroyParams,
    TaskAutomationsListQueryParams,
    TaskAutomationsPartialUpdateBody,
    TaskAutomationsPartialUpdateParams,
    TaskAutomationsRetrieveParams,
    TaskAutomationsRunCreateParams,
    TasksCreateBody,
    TasksDestroyParams,
    TasksListQueryParams,
    TasksPartialUpdateBody,
    TasksPartialUpdateParams,
    TasksRepositoryReadinessRetrieveQueryParams,
    TasksRetrieveParams,
    TasksRunsListParams,
    TasksRunsListQueryParams,
    TasksRunsRetrieveParams,
    TasksRunsSessionLogsRetrieveParams,
    TasksRunsSessionLogsRetrieveQueryParams,
} from '@/generated/tasks/api'
import { withPostHogUrl, type WithPostHogUrl } from '@/tools/tool-utils'
import type { Context, ToolBase, ZodObjectAny } from '@/tools/types'

const TasksListSchema = TasksListQueryParams

const tasksList = (): ToolBase<typeof TasksListSchema, WithPostHogUrl<Schemas.PaginatedTaskList>> => ({
    name: 'tasks-list',
    schema: TasksListSchema,
    handler: async (context: Context, params: z.infer<typeof TasksListSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.PaginatedTaskList>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/`,
            query: {
                created_by: params.created_by,
                internal: params.internal,
                limit: params.limit,
                offset: params.offset,
                organization: params.organization,
                origin_product: params.origin_product,
                repository: params.repository,
                stage: params.stage,
            },
        })
        return await withPostHogUrl(
            context,
            {
                ...result,
                results: await Promise.all(
                    (result.results ?? []).map((item) => withPostHogUrl(context, item, `/tasks/${item.id}`))
                ),
            },
            '/tasks'
        )
    },
})

const TasksRetrieveSchema = TasksRetrieveParams.omit({ project_id: true })

const tasksRetrieve = (): ToolBase<typeof TasksRetrieveSchema, WithPostHogUrl<Schemas.Task>> => ({
    name: 'tasks-retrieve',
    schema: TasksRetrieveSchema,
    handler: async (context: Context, params: z.infer<typeof TasksRetrieveSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.Task>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/${encodeURIComponent(String(params.id))}/`,
        })
        return await withPostHogUrl(context, result, `/tasks/${result.id}`)
    },
})

const TasksCreateSchema = TasksCreateBody

const tasksCreate = (): ToolBase<typeof TasksCreateSchema, WithPostHogUrl<Schemas.Task>> => ({
    name: 'tasks-create',
    schema: TasksCreateSchema,
    handler: async (context: Context, params: z.infer<typeof TasksCreateSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const body: Record<string, unknown> = {}
        if (params.title !== undefined) {
            body['title'] = params.title
        }
        if (params.title_manually_set !== undefined) {
            body['title_manually_set'] = params.title_manually_set
        }
        if (params.description !== undefined) {
            body['description'] = params.description
        }
        if (params.origin_product !== undefined) {
            body['origin_product'] = params.origin_product
        }
        if (params.repository !== undefined) {
            body['repository'] = params.repository
        }
        if (params.github_integration !== undefined) {
            body['github_integration'] = params.github_integration
        }
        if (params.signal_report !== undefined) {
            body['signal_report'] = params.signal_report
        }
        if (params.signal_report_task_relationship !== undefined) {
            body['signal_report_task_relationship'] = params.signal_report_task_relationship
        }
        if (params.json_schema !== undefined) {
            body['json_schema'] = params.json_schema
        }
        if (params.internal !== undefined) {
            body['internal'] = params.internal
        }
        if (params.ci_prompt !== undefined) {
            body['ci_prompt'] = params.ci_prompt
        }
        const result = await context.api.request<Schemas.Task>({
            method: 'POST',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/`,
            body,
        })
        return await withPostHogUrl(context, result, `/tasks/${result.id}`)
    },
})

const TasksPartialUpdateSchema = TasksPartialUpdateParams.omit({ project_id: true }).extend(
    TasksPartialUpdateBody.shape
)

const tasksPartialUpdate = (): ToolBase<typeof TasksPartialUpdateSchema, WithPostHogUrl<Schemas.Task>> => ({
    name: 'tasks-partial-update',
    schema: TasksPartialUpdateSchema,
    handler: async (context: Context, params: z.infer<typeof TasksPartialUpdateSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const body: Record<string, unknown> = {}
        if (params.title !== undefined) {
            body['title'] = params.title
        }
        if (params.title_manually_set !== undefined) {
            body['title_manually_set'] = params.title_manually_set
        }
        if (params.description !== undefined) {
            body['description'] = params.description
        }
        if (params.origin_product !== undefined) {
            body['origin_product'] = params.origin_product
        }
        if (params.repository !== undefined) {
            body['repository'] = params.repository
        }
        if (params.github_integration !== undefined) {
            body['github_integration'] = params.github_integration
        }
        if (params.signal_report !== undefined) {
            body['signal_report'] = params.signal_report
        }
        if (params.signal_report_task_relationship !== undefined) {
            body['signal_report_task_relationship'] = params.signal_report_task_relationship
        }
        if (params.json_schema !== undefined) {
            body['json_schema'] = params.json_schema
        }
        if (params.internal !== undefined) {
            body['internal'] = params.internal
        }
        if (params.ci_prompt !== undefined) {
            body['ci_prompt'] = params.ci_prompt
        }
        const result = await context.api.request<Schemas.Task>({
            method: 'PATCH',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/${encodeURIComponent(String(params.id))}/`,
            body,
        })
        return await withPostHogUrl(context, result, `/tasks/${result.id}`)
    },
})

const TasksDestroySchema = TasksDestroyParams.omit({ project_id: true })

const tasksDestroy = (): ToolBase<typeof TasksDestroySchema, unknown> => ({
    name: 'tasks-destroy',
    schema: TasksDestroySchema,
    handler: async (context: Context, params: z.infer<typeof TasksDestroySchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<unknown>({
            method: 'DELETE',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/${encodeURIComponent(String(params.id))}/`,
        })
        return result
    },
})

const TasksRepositoryReadinessRetrieveSchema = TasksRepositoryReadinessRetrieveQueryParams

const tasksRepositoryReadinessRetrieve = (): ToolBase<
    typeof TasksRepositoryReadinessRetrieveSchema,
    Schemas.RepositoryReadinessResponse
> => ({
    name: 'tasks-repository-readiness-retrieve',
    schema: TasksRepositoryReadinessRetrieveSchema,
    handler: async (context: Context, params: z.infer<typeof TasksRepositoryReadinessRetrieveSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.RepositoryReadinessResponse>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/repository_readiness/`,
            query: {
                refresh: params.refresh,
                repository: params.repository,
                window_days: params.window_days,
            },
        })
        return result
    },
})

const TasksRunsListSchema = TasksRunsListParams.omit({ project_id: true }).extend(TasksRunsListQueryParams.shape)

const tasksRunsList = (): ToolBase<typeof TasksRunsListSchema, WithPostHogUrl<Schemas.PaginatedTaskRunDetailList>> => ({
    name: 'tasks-runs-list',
    schema: TasksRunsListSchema,
    handler: async (context: Context, params: z.infer<typeof TasksRunsListSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.PaginatedTaskRunDetailList>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/${encodeURIComponent(String(params.task_id))}/runs/`,
            query: {
                limit: params.limit,
                offset: params.offset,
            },
        })
        return await withPostHogUrl(context, result, '/tasks')
    },
})

const TasksRunsRetrieveSchema = TasksRunsRetrieveParams.omit({ project_id: true })

const tasksRunsRetrieve = (): ToolBase<typeof TasksRunsRetrieveSchema, Schemas.TaskRunDetail> => ({
    name: 'tasks-runs-retrieve',
    schema: TasksRunsRetrieveSchema,
    handler: async (context: Context, params: z.infer<typeof TasksRunsRetrieveSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.TaskRunDetail>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/${encodeURIComponent(String(params.task_id))}/runs/${encodeURIComponent(String(params.id))}/`,
        })
        return result
    },
})

const TasksRunsSessionLogsRetrieveSchema = TasksRunsSessionLogsRetrieveParams.omit({ project_id: true }).extend(
    TasksRunsSessionLogsRetrieveQueryParams.shape
)

const tasksRunsSessionLogsRetrieve = (): ToolBase<typeof TasksRunsSessionLogsRetrieveSchema, unknown> => ({
    name: 'tasks-runs-session-logs-retrieve',
    schema: TasksRunsSessionLogsRetrieveSchema,
    handler: async (context: Context, params: z.infer<typeof TasksRunsSessionLogsRetrieveSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<unknown>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/tasks/${encodeURIComponent(String(params.task_id))}/runs/${encodeURIComponent(String(params.id))}/session_logs/`,
            query: {
                after: params.after,
                event_types: params.event_types,
                exclude_types: params.exclude_types,
                limit: params.limit,
                offset: params.offset,
            },
        })
        return result
    },
})

const TaskAutomationsListSchema = TaskAutomationsListQueryParams

const taskAutomationsList = (): ToolBase<
    typeof TaskAutomationsListSchema,
    WithPostHogUrl<Schemas.PaginatedTaskAutomationList>
> => ({
    name: 'task-automations-list',
    schema: TaskAutomationsListSchema,
    handler: async (context: Context, params: z.infer<typeof TaskAutomationsListSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.PaginatedTaskAutomationList>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/task_automations/`,
            query: {
                limit: params.limit,
                offset: params.offset,
            },
        })
        return await withPostHogUrl(
            context,
            {
                ...result,
                results: await Promise.all(
                    (result.results ?? []).map((item) => withPostHogUrl(context, item, `/tasks/${item.id}`))
                ),
            },
            '/tasks'
        )
    },
})

const TaskAutomationsCreateSchema = TaskAutomationsCreateBody

const taskAutomationsCreate = (): ToolBase<
    typeof TaskAutomationsCreateSchema,
    WithPostHogUrl<Schemas.TaskAutomation>
> => ({
    name: 'task-automations-create',
    schema: TaskAutomationsCreateSchema,
    handler: async (context: Context, params: z.infer<typeof TaskAutomationsCreateSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const body: Record<string, unknown> = {}
        if (params.name !== undefined) {
            body['name'] = params.name
        }
        if (params.prompt !== undefined) {
            body['prompt'] = params.prompt
        }
        if (params.repository !== undefined) {
            body['repository'] = params.repository
        }
        if (params.github_integration !== undefined) {
            body['github_integration'] = params.github_integration
        }
        if (params.cron_expression !== undefined) {
            body['cron_expression'] = params.cron_expression
        }
        if (params.timezone !== undefined) {
            body['timezone'] = params.timezone
        }
        if (params.template_id !== undefined) {
            body['template_id'] = params.template_id
        }
        if (params.enabled !== undefined) {
            body['enabled'] = params.enabled
        }
        const result = await context.api.request<Schemas.TaskAutomation>({
            method: 'POST',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/task_automations/`,
            body,
        })
        return await withPostHogUrl(context, result, `/tasks/${result.id}`)
    },
})

const SandboxListSchema = SandboxListQueryParams

const sandboxList = (): ToolBase<
    typeof SandboxListSchema,
    WithPostHogUrl<Schemas.PaginatedSandboxEnvironmentListList>
> => ({
    name: 'sandbox-list',
    schema: SandboxListSchema,
    handler: async (context: Context, params: z.infer<typeof SandboxListSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.PaginatedSandboxEnvironmentListList>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/sandbox_environments/`,
            query: {
                limit: params.limit,
                offset: params.offset,
            },
        })
        return await withPostHogUrl(
            context,
            {
                ...result,
                results: await Promise.all(
                    (result.results ?? []).map((item) => withPostHogUrl(context, item, `/tasks/${item.id}`))
                ),
            },
            '/tasks'
        )
    },
})

const SandboxRetrieveSchema = SandboxRetrieveParams.omit({ project_id: true })

const sandboxRetrieve = (): ToolBase<typeof SandboxRetrieveSchema, Schemas.SandboxEnvironment> => ({
    name: 'sandbox-retrieve',
    schema: SandboxRetrieveSchema,
    handler: async (context: Context, params: z.infer<typeof SandboxRetrieveSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.SandboxEnvironment>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/sandbox_environments/${encodeURIComponent(String(params.id))}/`,
        })
        return result
    },
})

const TaskAutomationsRetrieveSchema = TaskAutomationsRetrieveParams.omit({ project_id: true })

const taskAutomationsRetrieve = (): ToolBase<typeof TaskAutomationsRetrieveSchema, Schemas.TaskAutomation> => ({
    name: 'task-automations-retrieve',
    schema: TaskAutomationsRetrieveSchema,
    handler: async (context: Context, params: z.infer<typeof TaskAutomationsRetrieveSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.TaskAutomation>({
            method: 'GET',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/task_automations/${encodeURIComponent(String(params.id))}/`,
        })
        return result
    },
})

const TaskAutomationsPartialUpdateSchema = TaskAutomationsPartialUpdateParams.omit({ project_id: true }).extend(
    TaskAutomationsPartialUpdateBody.shape
)

const taskAutomationsPartialUpdate = (): ToolBase<
    typeof TaskAutomationsPartialUpdateSchema,
    Schemas.TaskAutomation
> => ({
    name: 'task-automations-partial-update',
    schema: TaskAutomationsPartialUpdateSchema,
    handler: async (context: Context, params: z.infer<typeof TaskAutomationsPartialUpdateSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const body: Record<string, unknown> = {}
        if (params.name !== undefined) {
            body['name'] = params.name
        }
        if (params.prompt !== undefined) {
            body['prompt'] = params.prompt
        }
        if (params.repository !== undefined) {
            body['repository'] = params.repository
        }
        if (params.github_integration !== undefined) {
            body['github_integration'] = params.github_integration
        }
        if (params.cron_expression !== undefined) {
            body['cron_expression'] = params.cron_expression
        }
        if (params.timezone !== undefined) {
            body['timezone'] = params.timezone
        }
        if (params.template_id !== undefined) {
            body['template_id'] = params.template_id
        }
        if (params.enabled !== undefined) {
            body['enabled'] = params.enabled
        }
        const result = await context.api.request<Schemas.TaskAutomation>({
            method: 'PATCH',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/task_automations/${encodeURIComponent(String(params.id))}/`,
            body,
        })
        return result
    },
})

const TaskAutomationsDestroySchema = TaskAutomationsDestroyParams.omit({ project_id: true })

const taskAutomationsDestroy = (): ToolBase<typeof TaskAutomationsDestroySchema, unknown> => ({
    name: 'task-automations-destroy',
    schema: TaskAutomationsDestroySchema,
    handler: async (context: Context, params: z.infer<typeof TaskAutomationsDestroySchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<unknown>({
            method: 'DELETE',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/task_automations/${encodeURIComponent(String(params.id))}/`,
        })
        return result
    },
})

const TaskAutomationsRunCreateSchema = TaskAutomationsRunCreateParams.omit({ project_id: true })

const taskAutomationsRunCreate = (): ToolBase<typeof TaskAutomationsRunCreateSchema, Schemas.TaskAutomation> => ({
    name: 'task-automations-run-create',
    schema: TaskAutomationsRunCreateSchema,
    handler: async (context: Context, params: z.infer<typeof TaskAutomationsRunCreateSchema>) => {
        const projectId = await context.stateManager.getProjectId()
        const result = await context.api.request<Schemas.TaskAutomation>({
            method: 'POST',
            path: `/api/projects/${encodeURIComponent(String(projectId))}/task_automations/${encodeURIComponent(String(params.id))}/run/`,
        })
        return result
    },
})

export const GENERATED_TOOLS: Record<string, () => ToolBase<ZodObjectAny>> = {
    'tasks-list': tasksList,
    'tasks-retrieve': tasksRetrieve,
    'tasks-create': tasksCreate,
    'tasks-partial-update': tasksPartialUpdate,
    'tasks-destroy': tasksDestroy,
    'tasks-repository-readiness-retrieve': tasksRepositoryReadinessRetrieve,
    'tasks-runs-list': tasksRunsList,
    'tasks-runs-retrieve': tasksRunsRetrieve,
    'tasks-runs-session-logs-retrieve': tasksRunsSessionLogsRetrieve,
    'task-automations-list': taskAutomationsList,
    'task-automations-create': taskAutomationsCreate,
    'sandbox-list': sandboxList,
    'sandbox-retrieve': sandboxRetrieve,
    'task-automations-retrieve': taskAutomationsRetrieve,
    'task-automations-partial-update': taskAutomationsPartialUpdate,
    'task-automations-destroy': taskAutomationsDestroy,
    'task-automations-run-create': taskAutomationsRunCreate,
}
