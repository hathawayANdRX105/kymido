//! H3 常量值锁定：`shared.rs` 的文案常量必须逐字等于原 rsx 中文字面量。
//!
//! 这是「零渲染漂移」（H1）的回归护栏——常量一旦在后续改动里被改写
//! （哪怕一个字符），本测试即失败。每个期望值即 78bee76 原件里 rsx
//! 元素体所用的原字面量。

use web_ui::shared as sh;

#[test]
fn btn_constants_match_original_literals() {
    assert_eq!(sh::BTN_SEND, "发送");
    assert_eq!(sh::BTN_STOP, "停止");
    assert_eq!(sh::BTN_CLOSE, "关闭");
    assert_eq!(sh::BTN_REMOVE, "移除");
    assert_eq!(sh::BTN_DELETE_SESSION, "删除会话");
    assert_eq!(sh::BTN_DELETE, "删除");
    assert_eq!(sh::BTN_ADD_IMAGE, "添加图片附件");
    assert_eq!(sh::BTN_REMOVE_SPACE, "移除项目");
    assert_eq!(sh::BTN_NEW_SESSION, "新会话");
    assert_eq!(sh::BTN_NEW_CHAT_IN_SPACE, "新建会话");
    assert_eq!(sh::BTN_SAVE_CONFIG, "保存配置");
    assert_eq!(sh::BTN_SAVE_MCP, "保存 MCP 配置");
    assert_eq!(sh::BTN_EXPAND_SIDEBAR, "展开侧边栏");
    assert_eq!(sh::BTN_COLLAPSE_SIDEBAR, "收起侧边栏");
    assert_eq!(sh::BTN_TASK_PANEL, "任务看板");
}

#[test]
fn lbl_constants_match_original_literals() {
    assert_eq!(sh::LBL_SESSIONS, "会话");
    assert_eq!(sh::LBL_STATS, "数据统计");
    assert_eq!(sh::LBL_SETTINGS, "设置");
    assert_eq!(sh::LBL_ABOUT, "关于");
    assert_eq!(sh::LBL_MODEL_CHANNEL, "模型与渠道");
    assert_eq!(sh::LBL_MCP_SERVERS, "MCP 服务器");
    assert_eq!(sh::LBL_THINKING_STRENGTH, "思考强度");
    assert_eq!(sh::LBL_THINKING, "思考");
    assert_eq!(sh::LBL_PICK_MODEL, "选择模型");
    assert_eq!(sh::LBL_PICK_SESSION, "选择会话快速切换");
    assert_eq!(sh::LBL_PLAN_REVIEW, "计划评审");
    // a2c675f 起按 aui 词族换名：Work Process 块头行分进行中/结束两形
    // （原 LBL_WORK_PROCESS），思考块头行分 Thinking/Thought（原 LBL_REASONING）。
    assert_eq!(sh::LBL_WORK_PROGRESSING, "Progressing");
    assert_eq!(sh::LBL_WORK_PROGRESS, "Progress");
    assert_eq!(sh::LBL_THINKING_EN, "Thinking");
    assert_eq!(sh::LBL_THOUGHT, "Thought");
    assert_eq!(sh::LBL_DESCRIPTION, "描述");
    assert_eq!(sh::LBL_ACCEPTANCE, "验收标准");
    assert_eq!(sh::LBL_RUNNING, "运行中");
    assert_eq!(sh::LBL_EXIT, "退出");
}

#[test]
fn msg_constants_match_original_literals() {
    assert_eq!(sh::MSG_EMPTY_CHAT_TITLE, "开始一个新的任务");
    assert_eq!(
        sh::MSG_EMPTY_CHAT_DESC,
        "在下方输入指令，Agent 将使用文件读写、bash 与代码编辑工具协助你完成。"
    );
    assert_eq!(sh::MSG_THINKING, "思考中");
    assert_eq!(sh::MSG_GENERATING, "正在生成回复...");
    assert_eq!(sh::MSG_ATTACHMENT_READING, "处理中…");
    assert_eq!(sh::MSG_NO_IMAGE_INPUT, "当前模型不支持图片输入");
    assert_eq!(sh::MSG_SEARCH_SESSION, "搜索会话 (⌘K)");
    assert_eq!(sh::MSG_SEARCH_PLACEHOLDER, "搜索会话名称或编号...");
    assert_eq!(
        sh::MSG_INPUT_PLACEHOLDER,
        "输入指令，Enter 发送，Shift+Enter 换行..."
    );
    assert_eq!(sh::MSG_TOOL_FAILED, "失败");
}

#[test]
fn opt_constants_match_original_thinking_levels() {
    assert_eq!(sh::OPT_THINKING_OFF, "关闭");
    assert_eq!(sh::OPT_THINKING_LIGHT, "轻量");
    assert_eq!(sh::OPT_THINKING_STANDARD, "标准");
    assert_eq!(sh::OPT_THINKING_DEEP, "深度");
}

#[test]
fn status_constants_match_original_badges() {
    assert_eq!(sh::STATUS_ALL, "全部");
    assert_eq!(sh::STATUS_IN_PROGRESS, "进行中");
    assert_eq!(sh::STATUS_OPEN, "待办");
    assert_eq!(sh::STATUS_DONE, "已完成");
    assert_eq!(sh::STATUS_BLOCKED, "已阻塞");
    assert_eq!(sh::STATUS_FAILED, "失败");
    assert_eq!(sh::STATUS_ABORTED, "已中止");
    assert_eq!(sh::STATUS_SUCCESS, "成功");
    assert_eq!(sh::STATUS_NO_COMPARE, "无可比窗口");
}

#[test]
fn ttl_constants_match_original_dialog_titles() {
    assert_eq!(sh::TTL_STATS, "数据统计");
    assert_eq!(sh::TTL_ABOUT, "关于 kymido");
}
