#!/usr/bin/env python3
"""Synthetic document corpus for the semantic acceptance test (SEM-01..05).

    python sem-corpus.py <root> [count]

Writes `count` (default 10000) short documents in topic folders -- text, Markdown, DOCX,
XLSX names and picture names, English and Chinese, with realistic name noise -- plus the
target files the test looks for:
  Finance/2026Q2_budget.xlsx         (SEM-02, Chinese query "季度预算")
  Security/api-key-rotation.docx     (SEM-03, content "how to rotate api key")
  Notes/changing-note.txt            (SEM-05, rewritten by the test)
Only the Python standard library is used. The output is deterministic (seed 7).
"""
import os
import random
import sys
import zipfile

R = random.Random(7)

TOPICS = {
    "Finance": (["budget", "invoice", "expenses", "forecast", "payroll", "tax", "ledger"],
                ["The quarterly budget review shows spending on cloud services grew faster than planned.",
                 "Invoices from suppliers must be approved by the department head before payment.",
                 "We expect revenue to recover in the second half if the new pricing works.",
                 "Travel expenses were reimbursed after the receipts were checked.",
                 "The forecast assumes a five percent increase in headcount."],
                ["本季度预算执行情况良好，但云服务支出超出计划。", "供应商发票需经部门负责人审批后付款。"]),
    "Projects": (["roadmap", "plan", "milestones", "spec", "release", "backlog"],
                 ["The roadmap lists three milestones for the mobile app this year.",
                  "Release 2.4 is blocked by the payment integration and needs another week.",
                  "The team agreed to move the search feature into the next sprint.",
                  "Risks include the vendor delay and the missing test environment."],
                 ["产品路线图包含今年移动端的三个里程碑。", "下个迭代将完成搜索功能。"]),
    "Meetings": (["minutes", "meeting", "standup", "retro", "agenda"],
                 ["In the weekly meeting we discussed hiring two engineers and a designer.",
                  "Action items: update the onboarding guide and book the training room.",
                  "The retrospective found that code reviews take too long.",
                  "Next meeting will review the customer feedback survey."],
                 ["会议纪要：讨论了招聘两名工程师和一名设计师。", "下次会议将回顾客户反馈调查。"]),
    "Travel": (["trip", "itinerary", "flight", "hotel", "visa"],
               ["The flight to Tokyo leaves at nine and the hotel is near the station.",
                "Remember to bring the passport, the charger and an umbrella.",
                "The itinerary includes two days in Kyoto and a visit to the museum.",
                "Book the train tickets a week before the trip."],
               ["行程包括在京都停留两天并参观博物馆。", "出发前一周预订火车票。"]),
    "Recipes": (["recipe", "soup", "cake", "salad", "dinner"],
                ["Slowly cook the tomatoes with garlic and olive oil for twenty minutes.",
                 "Mix flour, sugar and eggs, then bake the cake at 180 degrees.",
                 "Serve the salad with lemon dressing and toasted seeds.",
                 "The soup tastes better the next day."],
                ["番茄和大蒜用橄榄油慢炖二十分钟。", "蛋糕在一百八十度烤三十分钟。"]),
    "HR": (["policy", "onboarding", "vacation", "review", "benefits"],
           ["New employees receive their laptop and accounts on the first day.",
            "Vacation requests should be submitted two weeks in advance.",
            "Performance reviews take place every six months.",
            "The benefits package includes health insurance and a gym subsidy."],
           ["新员工入职第一天领取电脑和账号。", "休假申请需提前两周提交。"]),
    "Reports": (["report", "summary", "analysis", "statistics", "survey"],
                ["The customer survey shows satisfaction improved after the redesign.",
                 "Website traffic doubled during the spring campaign.",
                 "The analysis compares support tickets before and after the update.",
                 "Most users open the app in the evening."],
                ["客户调查显示改版后满意度提升。", "春季活动期间网站访问量翻倍。"]),
    "Notes": (["notes", "ideas", "todo", "journal", "draft"],
              ["Ideas for the garden: plant herbs near the kitchen window.",
               "Call the plumber about the leaking tap on Monday.",
               "Read the book about habits and write a short summary.",
               "Buy a birthday present for Anna before Friday."],
              ["周一给水管工打电话修漏水的水龙头。", "周五前给安娜买生日礼物。"]),
}
NOISE = ["final", "v2", "v3", "draft", "copy", "old", "new", "2025", "2026", "Q1", "Q3", "rev", "signed"]


def docx(path, paragraphs):
    body = "".join(f"<w:p><w:r><w:t xml:space=\"preserve\">{p}</w:t></w:r></w:p>" for p in paragraphs)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml",
                   '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
                   '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
                   '<Default Extension="xml" ContentType="application/xml"/>'
                   '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
        z.writestr("_rels/.rels",
                   '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                   '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
        z.writestr("word/document.xml",
                   '<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
                   f"<w:body>{body}</w:body></w:document>")


def name_for(words, i):
    w = R.sample(words, k=min(2, len(words)))
    style = R.randrange(5)
    if style == 0:
        return f"{w[0]}_{R.choice(NOISE)}_{i}"
    if style == 1:
        return f"{R.randint(2019, 2026)}-{R.randint(1, 12):02d}-{R.randint(1, 28):02d} {w[0]} {w[-1]}"
    if style == 2:
        return f"{w[0].capitalize()}{w[-1].capitalize()}{i}"
    if style == 3:
        return f"{w[0]}-{w[-1]}-{R.choice(NOISE)}-{i}"
    return f"{w[0]} {i}"


def main():
    root = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 10000
    for t in list(TOPICS) + ["Security", "Photos"]:
        os.makedirs(os.path.join(root, t), exist_ok=True)
    topics = list(TOPICS.items())
    for i in range(count):
        folder, (words, en, zh) = topics[i % len(topics)]
        kind = R.random()
        base = os.path.join(root, folder, name_for(words, i))
        sentences = R.sample(en, k=R.randint(2, len(en))) + (R.sample(zh, k=1) if R.random() < 0.2 else [])
        if kind < 0.45:
            open(base + ".txt", "w", encoding="utf-8").write(" ".join(sentences) + "\n")
        elif kind < 0.65:
            open(base + ".md", "w", encoding="utf-8").write(f"# {words[0].title()}\n\n" + "\n\n".join(sentences) + "\n")
        elif kind < 0.8:
            docx(base + ".docx", sentences)
        elif kind < 0.9:
            open(base + ".xlsx", "wb").write(b"PK\x05\x06" + b"\x00" * 18)
        else:
            photo = os.path.join(root, "Photos", f"IMG_{20200101 + i}_{R.randint(1000, 9999)}.jpg")
            open(photo, "wb").write(b"\xff\xd8\xff\xd9")
    # Targets and distractors.
    open(os.path.join(root, "Finance", "2026Q2_budget.xlsx"), "wb").write(b"PK\x05\x06" + b"\x00" * 18)
    for n in ["2025Q4_expenses.xlsx", "payroll_march.xlsx", "travel_budget_japan.txt", "annual_report_2025.docx"]:
        p = os.path.join(root, "Finance", n)
        if n.endswith(".docx"):
            docx(p, ["Annual report with revenue, costs and outlook."])
        elif n.endswith(".txt"):
            open(p, "w", encoding="utf-8").write("Budget for the Japan trip: flights, hotels and rail passes.\n")
        else:
            open(p, "wb").write(b"PK\x05\x06" + b"\x00" * 18)
    docx(os.path.join(root, "Security", "api-key-rotation.docx"), [
        "Security handbook for the developer platform.",
        "Passwords must be at least twelve characters long and are never shared by email.",
        "To rotate an API key, open the developer console, select Keys, choose the key and press Rotate. "
        "The old key keeps working for 24 hours so that services can switch to the new one.",
        "Report lost laptops to the help desk immediately.",
    ])
    docx(os.path.join(root, "Security", "password-policy.docx"), ["Passwords expire every 90 days and must not be reused."])
    open(os.path.join(root, "Notes", "changing-note.txt"), "w", encoding="utf-8").write(
        "Grocery list: milk, eggs, bread and butter for the weekend.\n")
    print(f"wrote {count} documents and the target files under {root}")


if __name__ == "__main__":
    main()
