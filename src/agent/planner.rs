//! Task planner — breaks down complex tasks into manageable steps.
//!
//! The planner analyzes the user's request and creates a structured plan.
//! For simple tasks, this may be a single step; for complex ones,
//! it creates a dependency-ordered list of sub-tasks.

/// A single step in an execution plan.
#[derive(Debug, Clone)]
pub struct PlanStep {
    /// Step number (1-indexed)
    pub number: usize,
    /// Description of what this step accomplishes
    pub description: String,
    /// Dependencies on other step numbers (0 means no dependency)
    pub depends_on: Vec<usize>,
    /// Current status
    pub status: StepStatus,
}

/// Status of a plan step.
#[derive(Debug, Clone, PartialEq)]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Failed(String),
    Skipped,
}

/// A complete task execution plan.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The original task description
    pub task: String,
    /// Ordered list of steps
    pub steps: Vec<PlanStep>,
    /// Overall plan status
    pub status: PlanStatus,
    /// Maximum number of execution turns budgeted for this plan
    pub max_turns: u32,
}

impl Plan {
    /// Render the plan as a readable block for the model (s03-style
    /// plan echo: state lives outside the conversation but is shown).
    pub fn render(&self) -> String {
        if self.steps.is_empty() {
            return format!("Task: {}\n(no steps)", self.task);
        }
        let steps = self
            .steps
            .iter()
            .map(|s| {
                let mark = match s.status {
                    StepStatus::Pending => "[ ]",
                    StepStatus::InProgress => "[>]",
                    StepStatus::Completed => "[x]",
                    StepStatus::Failed(_) => "[!]",
                    StepStatus::Skipped => "[-]",
                };
                let deps = if s.depends_on.is_empty() {
                    String::new()
                } else {
                    let ids = s.depends_on.iter().map(|d| d.to_string()).collect::<Vec<_>>();
                    format!(" (depends on: {})", ids.join(", "))
                };
                format!("{} #{}\t{}{}", mark, s.number, s.description, deps)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("Task: {}\n{}\n\nPlan budget: {} turns", self.task, steps, self.max_turns)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanStatus {
    Draft,
    Executing,
    Completed,
    Failed,
}

/// The planner is responsible for creating and managing task plans.
#[derive(Debug)]
pub struct Planner {
    max_turns: u32,
}

impl Planner {
    /// Create a new planner.
    pub fn new(max_turns: u32) -> Self {
        Self { max_turns }
    }

    /// Create a simple plan for a task.
    ///
    /// For now, this creates a single-step plan. In the future, this will
    /// use the LLM to decompose complex tasks into sub-steps.
    pub fn create_plan(&self, task: &str) -> Plan {
        Plan {
            task: task.to_string(),
            steps: vec![PlanStep {
                number: 1,
                description: format!("Execute: {}", task),
                depends_on: vec![],
                status: StepStatus::Pending,
            }],
            status: PlanStatus::Draft,
            max_turns: self.max_turns,
        }
    }

    /// Get the next pending step that is ready to execute.
    pub fn next_step<'a>(&self, plan: &'a Plan) -> Option<&'a PlanStep> {
        plan.steps
            .iter()
            .find(|s| s.status == StepStatus::Pending && self.dependencies_met(s, plan))
    }

    /// Check if all dependencies for a step are satisfied.
    fn dependencies_met(&self, step: &PlanStep, plan: &Plan) -> bool {
        step.depends_on.iter().all(|dep_num| {
            plan.steps
                .iter()
                .find(|s| s.number == *dep_num)
                .map(|s| s.status == StepStatus::Completed)
                .unwrap_or(true)
        })
    }

    /// Get progress as a percentage.
    pub fn progress(&self, plan: &Plan) -> f32 {
        if plan.steps.is_empty() {
            return 100.0;
        }
        let completed = plan
            .steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped))
            .count();
        (completed as f32 / plan.steps.len() as f32) * 100.0
    }
}
