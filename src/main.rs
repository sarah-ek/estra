use clap::Parser;
use color_eyre::{config::HookBuilder, eyre};
use colorgrad::viridis;
use crossterm::{
    event::{self, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use diol::Picoseconds;
use ratatui::{
    prelude::*,
    symbols::border,
    widgets::{self, Axis, Block, Borders, Dataset, Paragraph},
};
use std::{
    io::{self, stdout, Stdout},
    panic,
    path::PathBuf,
};

#[derive(Debug)]
struct App {
    path: PathBuf,
    result: diol::BenchResult,
    group_idx: usize,
    arg_idx_per_group: Vec<usize>,
    exit: bool,
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn mean(timings: &[Picoseconds]) -> Picoseconds {
    let sum = timings.iter().map(|x| x.0).sum::<i128>();
    let mean = Picoseconds(sum.checked_div(timings.len() as i128).unwrap_or(0));
    mean
}

// taken from the stdlib
pub const fn isqrt(this: u128) -> u128 {
    if this < 2 {
        return this;
    }

    // The algorithm is based on the one presented in
    // <https://en.wikipedia.org/wiki/Methods_of_computing_square_roots#Binary_numeral_system_(base_2)>
    // which cites as source the following C code:
    // <https://web.archive.org/web/20120306040058/http://medialab.freaknet.org/martin/src/sqrt/sqrt.c>.

    let mut op = this;
    let mut res = 0;
    let mut one = 1 << (this.ilog2() & !1);

    while one != 0 {
        if op >= res + one {
            op -= res + one;
            res = (res >> 1) + one;
        } else {
            res >>= 1;
        }
        one >>= 2;
    }

    res
}

fn stddev(timings: &[Picoseconds], mean: Picoseconds) -> Picoseconds {
    let count = timings.len();
    let variance = if count <= 1 {
        0
    } else {
        timings
            .iter()
            .map(|x| {
                let diff = x.0 - mean.0;
                diff * diff
            })
            .sum::<i128>()
            / (count as i128 - 1)
    };
    Picoseconds(isqrt(variance as u128) as i128)
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(border::PLAIN);
        let counter_text = Text::from(vec![Line::from(vec!["Value: ".into(), "yahoo!".yellow()])]);

        Paragraph::new(counter_text)
            .centered()
            .block(block)
            .render(area, buf);
    }
}

impl App {
    pub fn new(path: PathBuf) -> io::Result<Self> {
        let file = std::fs::File::open(&path)?;
        let result: diol::BenchResult = serde_json::from_reader(file)?;
        let group_count = result.groups.len();
        let app = App {
            path,
            result,
            group_idx: 0,
            arg_idx_per_group: vec![0; group_count],
            exit: false,
        };
        Ok(app)
    }

    /// runs the application's main loop until the user quits
    pub fn run(&mut self, terminal: &mut Tui) -> io::Result<()> {
        while !self.exit {
            terminal.draw(|frame| self.render_frame(frame))?;
            self.handle_events()?;
        }
        Ok(())
    }

    fn render_frame(&self, frame: &mut Frame) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(frame.size());

        let block = Block::new().borders(Borders::all());

        let list = widgets::List::new(self.result.groups.iter().map(|group| {
            group
                .function
                .iter()
                .map(|f| &*f.name)
                .collect::<Vec<_>>()
                .join(" | ")
        }))
        .highlight_style(Style::default().black().on_white())
        .block(block.clone());
        let mut list_state = widgets::ListState::default();
        if !self.result.groups.is_empty() {
            list_state = list_state.with_selected(Some(self.group_idx))
        }

        if let Some(group) = self.result.groups.get(self.group_idx) {
            let split_layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(layout[0]);

            let args_len = match &group.args {
                diol::BenchArgs::Named(args) => args.len(),
                diol::BenchArgs::Plot(args) => args.len(),
            };

            let mut header = vec!["args"];

            let mut timings =
                vec![vec![(Picoseconds(0), Picoseconds(0)); group.function.len()]; args_len];
            for (func_idx, func) in group.function.iter().enumerate() {
                header.push(&func.name);
                for (arg_idx, arg) in func.timings.iter().enumerate() {
                    let mean = mean(arg);
                    let stddev = stddev(arg, mean);
                    timings[arg_idx][func_idx] = (mean, stddev);
                }
            }

            match &group.args {
                diol::BenchArgs::Named(args) => {
                    let data = widgets::Table::default().block(block.clone()).rows(
                        args.iter()
                            .map(|f| f.clone())
                            .zip(timings.iter().map(|timings| {
                                timings
                                    .iter()
                                    .map(|(mean, stddev)| format!("{mean:?} ± {stddev:?}"))
                            }))
                            .map(|(name, data)| std::iter::once(name).chain(data))
                            .map(widgets::Row::new),
                    );
                    let mut data_state = widgets::TableState::new();

                    let data = data
                        .header(widgets::Row::new(header))
                        .widths(std::iter::once(Constraint::Min(14)).chain(
                            std::iter::repeat(Constraint::Min(10)).take(group.function.len()),
                        ))
                        .highlight_style(Style::default().black().on_white());

                    if !args.is_empty() {
                        data_state =
                            data_state.with_selected(Some(self.arg_idx_per_group[self.group_idx]));
                    }

                    frame.render_stateful_widget(list, layout[0], &mut list_state);
                    frame.render_stateful_widget(data, layout[1], &mut data_state);
                }
                diol::BenchArgs::Plot(args) => {
                    let data = widgets::Table::default().block(block.clone()).rows(
                        args.iter()
                            .map(|f| format!("{f:?}"))
                            .zip(timings.iter().map(|timings| {
                                timings
                                    .iter()
                                    .map(|(mean, stddev)| format!("{mean:?} ± {stddev:?}"))
                            }))
                            .map(|(name, data)| std::iter::once(name).chain(data))
                            .map(widgets::Row::new),
                    );
                    let mut data_state = widgets::TableState::new();

                    let data = data
                        .header(widgets::Row::new(header))
                        .widths(std::iter::once(Constraint::Min(14)).chain(
                            std::iter::repeat(Constraint::Min(10)).take(group.function.len()),
                        ))
                        .highlight_style(Style::default().black().on_white());

                    if !args.is_empty() {
                        data_state =
                            data_state.with_selected(Some(self.arg_idx_per_group[self.group_idx]));
                    }

                    let mut chart_data = vec![];

                    let mut xmin = 0.0f64;
                    let mut xmax = 0.0f64;
                    let mut ymin = 0.0f64;
                    let mut ymax = 0.0f64;

                    for func in &group.function {
                        chart_data.push(
                            args.iter()
                                .zip(func.timings.iter().map(|timings| mean(timings)))
                                .map(|(x, y)| {
                                    let (x, y) = (x.0 as f64, (y.0 as f64) * 1e-12);
                                    xmin = xmin.min(x);
                                    xmax = xmin.max(x);
                                    ymin = ymin.min(y);
                                    ymax = ymin.max(y);
                                    (x, y)
                                })
                                .collect::<Vec<_>>(),
                        )
                    }

                    let mut datasets = vec![];
                    for (idx, (func, chart_data)) in
                        std::iter::zip(&group.function, &chart_data).enumerate()
                    {
                        let color = viridis();
                        let color = if group.function.len() <= 1 {
                            color.at(0.5)
                        } else {
                            color.at(idx as f64 / (group.function.len() - 1) as f64)
                        };
                        datasets.push(
                            Dataset::default()
                                .name(&*func.name)
                                .graph_type(widgets::GraphType::Line)
                                .marker(Marker::Braille)
                                .data(chart_data)
                                .style(Style::default().fg(Color::Rgb(
                                    (color.r * 255.0) as u8,
                                    (color.g * 255.0) as u8,
                                    (color.b * 255.0) as u8,
                                ))),
                        );
                    }

                    // Create the X axis and define its properties
                    let x_axis = Axis::default()
                        .title("X Axis".red())
                        .style(Style::default().white())
                        .bounds([xmin, xmax])
                        .labels(vec![
                            format!("{xmin}").into(),
                            format!("{}", (xmin + xmax) / 2.0).into(),
                            format!("{xmax}").into(),
                        ]);

                    // Create the Y axis and define its properties
                    let y_axis = Axis::default()
                        .title("Y Axis".red())
                        .style(Style::default().white())
                        .bounds([ymin, ymax])
                        .labels(vec![
                            format!("{ymin:.2e}").into(),
                            format!("{:.2e}", (ymin + ymax) / 2.0).into(),
                            format!("{ymax:.2e}").into(),
                        ]);

                    let chart = widgets::Chart::new(datasets)
                        .on_dark_gray()
                        .block(block.clone())
                        .x_axis(x_axis)
                        .y_axis(y_axis)
                        .hidden_legend_constraints((Constraint::Min(0), Constraint::Ratio(1, 4)))
                        .legend_position(Some(widgets::LegendPosition::TopLeft));

                    frame.render_stateful_widget(list, split_layout[0], &mut list_state);
                    frame.render_widget(chart, split_layout[1]);
                    frame.render_stateful_widget(data, layout[1], &mut data_state);
                }
            }
        } else {
            frame.render_stateful_widget(list, layout[0], &mut list_state);
            frame.render_widget(widgets::Block::new().borders(Borders::all()), layout[1]);
        }
    }

    fn handle_events(&mut self) -> io::Result<()> {
        match event::read()? {
            // it's important to check that the event is a key press event as
            // crossterm also emits key release and repeat events on Windows.
            event::Event::Key(key_event) if key_event.kind == event::KeyEventKind::Press => {
                self.handle_key_event(key_event)
            }
            _ => {}
        };
        Ok(())
    }

    fn handle_key_event(&mut self, key_event: event::KeyEvent) {
        if self.result.groups.is_empty() {
            return;
        }

        match key_event.code {
            KeyCode::Char('q') => self.exit(),
            KeyCode::Char('k') => {
                if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                    self.arg_idx_per_group[self.group_idx] =
                        self.arg_idx_per_group[self.group_idx].saturating_sub(1);
                } else {
                    self.group_idx = self.group_idx.saturating_sub(1);
                }
            }
            KeyCode::Char('j') => {
                if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                    self.arg_idx_per_group[self.group_idx] = Ord::min(
                        match &self.result.groups[self.group_idx].args {
                            diol::BenchArgs::Named(args) => args.len(),
                            diol::BenchArgs::Plot(args) => args.len(),
                        }
                        .saturating_sub(1),
                        self.arg_idx_per_group[self.group_idx] + 1,
                    );
                } else {
                    self.group_idx = Ord::min(self.result.groups.len() - 1, self.group_idx + 1)
                }
            }
            KeyCode::Char('r') => {
                if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                    *self = Self::new(std::mem::take(&mut self.path)).unwrap()
                }
            }
            _ => {}
        }
    }

    fn exit(&mut self) {
        self.exit = true;
    }
}

fn main() -> eyre::Result<()> {
    #[derive(Parser)]
    struct Clap {
        path: PathBuf,
    }
    let clap = Clap::parse();

    install_hooks()?;
    let mut tui = init_tui()?;

    let mut app = App::new(clap.path)?;
    app.run(&mut tui)?;

    Ok(())
}

pub fn init_tui() -> io::Result<Tui> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout()))
}

pub fn restore_tui() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// This replaces the standard color_eyre panic and error hooks with hooks that
/// restore the terminal before printing the panic or error.
pub fn install_hooks() -> color_eyre::Result<()> {
    let (panic_hook, eyre_hook) = HookBuilder::default().into_hooks();

    // convert from a color_eyre PanicHook to a standard panic hook
    let panic_hook = panic_hook.into_panic_hook();
    panic::set_hook(Box::new(move |panic_info| {
        restore_tui().unwrap();
        panic_hook(panic_info);
    }));

    // convert from a color_eyre EyreHook to a eyre ErrorHook
    let eyre_hook = eyre_hook.into_eyre_hook();
    eyre::set_hook(Box::new(
        move |error: &(dyn std::error::Error + 'static)| {
            restore_tui().unwrap();
            eyre_hook(error)
        },
    ))?;

    Ok(())
}